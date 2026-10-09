//! Tool-cell tests (spec §6). Expected strings are built through
//! `pacode_render::text` — no display literals inline.

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
        streamed: None,
    }
}

/// A running call with the output it has produced so far.
fn running<'a>(name: &'a str, title: &'a str, streamed: &'a str) -> ToolCallView<'a> {
    ToolCallView {
        name,
        title,
        status: ToolStatus::Running,
        preview: "",
        diff: None,
        duration_ms: None,
        streamed: Some(crate::state::transcript::ToolStreamView {
            text: streamed,
            dropped: 0,
        }),
    }
}

fn running_capped<'a>(streamed: &'a str, dropped: usize) -> ToolCallView<'a> {
    ToolCallView {
        streamed: Some(crate::state::transcript::ToolStreamView {
            text: streamed,
            dropped,
        }),
        ..running("bash", "bash cargo build", streamed)
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

/// Body rows are the ones hanging on the ` │ ` gutter.
fn body_rows<'a>(text: &'a [String], opts: &RenderOptions) -> Vec<&'a str> {
    let g = format!(" {} ", opts.glyphs.vline);
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

    let label = rtext::tool_label("bash").unwrap_or("bash").to_uppercase();
    let ok = opts.glyphs.ok;
    let dur = format_duration_ms(400);
    // ` ✓ BASH  docker ps` — the badge, then the arg in normal weight.
    let expected_left = format!(" {ok} {label}  docker ps");
    assert!(header.starts_with(&expected_left), "{header}");
    assert_eq!(lines[0].spans[0].style, opts.theme.badge(opts.theme.green));
    assert_eq!(lines[0].spans[2].style, opts.theme.fg);

    // Genuinely flush right: the row is exactly `width` wide and ends with the
    // duration — the exit code moved into the `└` footer.
    assert_eq!(display_width(header), 80, "{header}");
    assert!(header.ends_with(&dur), "{header}");

    // `│ $ docker ps`, then the output; the exit-code line stays out of the body.
    let body = body_rows(&text, &opts);
    assert_eq!(body.len(), 2, "{body:?}");
    assert!(body[0].contains("$ docker ps"), "{}", body[0]);
    assert!(body[1].contains("container-a"), "{}", body[1]);

    // `└ Exited with code 0` closes the gutter.
    let corner = format!(" {} ", opts.glyphs.vcorner);
    let footer = text
        .iter()
        .find(|l| l.starts_with(&corner))
        .expect("a footer row on the └ elbow");
    assert!(footer.contains("Exited with code 0"), "{footer}");
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
    // The arg yields first: the badge and the duration both survive.
    assert!(
        header.starts_with(&format!(" {} READ ", opts.glyphs.ok)),
        "{header}"
    );
    assert!(header.ends_with(&format_duration_ms(40)), "{header}");
    assert!(header.contains(opts.glyphs.ellipsis), "{header}");
}

#[test]
fn the_badge_tone_follows_the_outcome() {
    let opts = RenderOptions::new(60, false);
    for (status, tone) in [
        (ToolStatus::Running, opts.theme.accent),
        (ToolStatus::Ok, opts.theme.green),
        (ToolStatus::Error, opts.theme.red),
        (ToolStatus::Denied, opts.theme.dim),
        (ToolStatus::Backgrounded, opts.theme.violet),
    ] {
        let lines = render(&call("read", "read x", status, "x"), 60, &opts, 0, false);
        assert_eq!(
            lines[0].spans[0].style,
            opts.theme.badge(tone),
            "{status:?}"
        );
    }
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

    let fail = opts.glyphs.fail;
    let dur = format_duration_ms(12_000);
    // `✗` in the badge; the duration flush right.
    assert!(text[0].starts_with(&format!(" {fail} ")), "{}", text[0]);
    assert!(text[0].ends_with(&dur), "{}", text[0]);
    let joined = text.join("\n");
    assert!(
        joined.contains("Exited with code 1"),
        "the `└` footer carries the code: {joined}"
    );
    assert!(
        !joined.contains("[exit code"),
        "the exit marker is consumed by the footer, not the body"
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
        assert!(content[0].contains(opts.glyphs.fail), "{}", content[0]);
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

    // The gutter is painted by US — not by the program.
    assert_eq!(lines[1].spans[0].style, opts.theme.dim);
    assert_eq!(
        lines[1].spans[0].content,
        format!(" {} ", opts.glyphs.vline)
    );
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
    assert!(text[0].starts_with(" + BASH  cargo test"), "{}", text[0]);
    assert!(text[0].ends_with("40ms"), "{}", text[0]);
    let body = body_rows(&text, &opts);
    assert!(!body.is_empty());
    assert!(body.iter().all(|l| l.starts_with(" | ")), "{body:?}");
    let corner = format!(" {} ", opts.glyphs.vcorner);
    assert!(
        text.iter()
            .any(|l| l.starts_with(&corner) && l.contains("Exited with code 0")),
        "{text:?}"
    );
}

#[test]
fn denied_calls_say_denied_on_a_dim_badge() {
    let opts = RenderOptions::new(80, false);
    let c = call("bash", "bash rm -rf /", ToolStatus::Denied, "Denied");
    let lines = render(&c, 80, &opts, 0, false);
    let text = text_of(&lines);
    assert!(text[0].ends_with("denied"), "{}", text[0]);
    // A refused call is quiet, and a denied bash call still shows what it
    // would have run.
    assert_eq!(lines[0].spans[0].style, opts.theme.badge(opts.theme.dim));
    assert!(text[0].starts_with(&format!(" {} BASH", opts.glyphs.disabled)));
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
    let bg = rtext::in_background();
    let dur = format_duration_ms(107_000);
    let sep = opts.glyphs.dot_sep;
    assert!(
        text[0].starts_with(&format!(" {running} BASH")),
        "{}",
        text[0]
    );
    assert!(text[0].ends_with(&format!("{bg}{sep}{dur}")), "{}", text[0]);
    assert_eq!(lines[0].spans[0].style, opts.theme.badge(opts.theme.violet));
    let body = body_rows(&text, &opts);
    assert!(
        body.iter()
            .any(|l| l.contains("task t1 running in background")),
        "{body:?}"
    );
}

#[test]
fn a_running_write_shows_the_content_it_is_writing() {
    // The whole point of streaming a writing tool: the user watches the file
    // arrive. A running call whose arguments are the content must draw all of
    // it, not the first line of it.
    let opts = RenderOptions::new(80, false);
    let mut c = running("write", "write x.rs", "");
    c.title = "write x.rs";
    c.preview = "";
    c.streamed = None;
    // Before anything arrives the row shows the target and the incomplete-tail
    // cursor — it is visibly in flight, and it is not showing content.
    let empty_text = text_of(&render(&c, 80, &opts, 0, false));
    let body = body_rows(&empty_text, &opts).join("\n");
    assert!(body.contains("x.rs"), "the target is missing: {body:?}");
    assert!(
        !body.contains("println"),
        "nothing has arrived yet, so no content may be drawn: {body:?}"
    );

    c.preview = r#"{"path":"x.rs","content":"fn main() {\n    println!(\"hi\");\n}"}"#;
    let text = text_of(&render(&c, 80, &opts, 0, false));
    let joined = body_rows(&text, &opts).join("\n");
    for expected in ["fn main()", "println!"] {
        assert!(
            joined.contains(expected),
            "the content being written is missing {expected:?}: {joined:?}"
        );
    }
}

#[test]
fn running_calls_spin_on_the_anim_frame() {
    let opts = RenderOptions::new(80, false);
    let mut c = call("bash", "bash cargo test", ToolStatus::Running, "");
    c.duration_ms = Some(12_000);
    let header_at =
        |frame: u64, o: &RenderOptions| text_of(&render(&c, 80, o, frame, false))[0].clone();
    assert!(
        header_at(0, &opts).contains("⠋ BASH"),
        "{}",
        header_at(0, &opts)
    );
    assert!(
        header_at(1, &opts).contains("⠙ BASH"),
        "{}",
        header_at(1, &opts)
    );
    assert!(
        header_at(10, &opts).contains("⠋ BASH"),
        "{}",
        header_at(10, &opts)
    );
    // The elapsed time sits at the right edge, after the swept word.
    assert!(
        header_at(0, &opts).contains("12s"),
        "{}",
        header_at(0, &opts)
    );

    let ascii = RenderOptions::new(80, true);
    assert!(
        header_at(0, &ascii).contains("- BASH"),
        "{}",
        header_at(0, &ascii)
    );
    assert!(
        header_at(1, &ascii).contains("\\ BASH"),
        "{}",
        header_at(1, &ascii)
    );
    assert!(
        header_at(2, &ascii).contains("| BASH"),
        "{}",
        header_at(2, &ascii)
    );
    assert!(
        header_at(3, &ascii).contains("/ BASH"),
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
    let label = rtext::tool_label("read_file")
        .unwrap_or("read_file")
        .to_uppercase();
    assert!(
        text[0].contains(&format!("{label}  crates/x.rs")),
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
    // arg row + EXPANDED_MAX_LINES wrapped rows + the truncation marker.
    assert!(
        body.len() <= EXPANDED_MAX_LINES + 2,
        "an enormous output must stay bounded: {}",
        body.len()
    );
    assert!(
        body.last().expect("last").contains("output truncated"),
        "{:?}",
        body.last()
    );
}

/// Body-row text without the `│ ` gutter — glued back together it must hold
/// every piece a wrap split mid-word.
fn glued_body(text: &[String], opts: &RenderOptions) -> String {
    let g = format!(" {} ", opts.glyphs.vline);
    body_rows(text, opts)
        .iter()
        .map(|l| l[g.len()..].to_string())
        .collect()
}

#[test]
fn expanded_bash_wraps_the_whole_command() {
    let opts = RenderOptions::new(40, false);
    let cmd = "find crates -name '*.rs' -exec grep -n expand {} +";
    let title = format!("bash {cmd}");
    let c = call("bash", &title, ToolStatus::Ok, "one");
    let lines = render(&c, 40, &opts, 0, true);
    let text = text_of(&lines);
    let body = body_rows(&text, &opts);
    // The collapsed `$` row truncates; expanded wraps the command across
    // gutter rows so every piece is on screen.
    assert!(body.len() > 2, "{body:?}");
    assert!(body[0].contains("$ find crates"), "{}", body[0]);
    // Wrap drops the break-point space and re-indents, so compare the pieces
    // glued without spaces.
    let glued = glued_body(&text, &opts).replace(' ', "");
    assert!(glued.contains(&cmd.replace(' ', "")), "{glued}");
    assert!(glued.contains("one"), "{glued}");
    assert!(
        body.iter().all(|l| !l.contains('…')),
        "expanded rows wrap instead of being cut: {body:?}"
    );
}

#[test]
fn expanded_non_bash_spells_out_the_full_arg() {
    let opts = RenderOptions::new(40, false);
    let arg = "a/very/long/path/that/does/not/fit/inside/forty/cells/at/all.rs";
    let title = format!("read_file {arg}");
    let c = call("read_file", &title, ToolStatus::Ok, "1 line");
    let lines = render(&c, 40, &opts, 0, true);
    let text = text_of(&lines);
    // The header stays a truncated one-line summary…
    assert!(!text[0].contains(arg), "{}", text[0]);
    // …while the expanded body wraps the whole argument before the output.
    let glued = glued_body(&text, &opts);
    assert!(glued.contains(arg), "{glued}");
    assert!(glued.contains("1 line"), "{glued}");
}

#[test]
fn a_denied_cell_expands_to_the_full_refused_command() {
    let opts = RenderOptions::new(30, false);
    let c = call(
        "bash",
        "bash rm -rf /very/long/path/that/wont/fit",
        ToolStatus::Denied,
        "Denied",
    );
    let lines = render(&c, 30, &opts, 0, true);
    let text = text_of(&lines);
    let glued = glued_body(&text, &opts).replace(' ', "");
    assert!(
        glued.contains("rm-rf/very/long/path/that/wont/fit"),
        "{glued}"
    );
    assert!(glued.contains("Denied"), "{glued}");
}

#[test]
fn a_diff_preview_renders_on_the_gutter() {
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
    // The quiet gutter fronts the diff renderer's output, and the header
    // still carries the +A −R stat.
    assert_eq!(lines[1].spans[0].style, opts.theme.dim);
    assert!(text[0].contains("+1"), "{}", text[0]);
    // No `$` line, no line cap — the diff renderer owns this body.
    assert!(body.iter().all(|l| !l.contains("$ ")), "{body:?}");
}

/// ``/Cyrillic at a byte offset the tool-name prefix would split —
/// `title_arg` must not slice mid-char (byte-index panic).
#[test]
fn title_arg_survives_a_multibyte_title() {
    // "ab": 'a','b' are bytes 0-1, '' is bytes 2-4 — a 4-byte name prefix
    // ("read") lands inside it. Slicing `title[..4]` would panic.
    let c = call("read", "ab output", ToolStatus::Ok, "");
    assert_eq!(title_arg(&c), "ab output");

    // A title genuinely starting with the name still strips it.
    let c = call("read", "read src/main.rs", ToolStatus::Ok, "");
    assert_eq!(title_arg(&c), "src/main.rs");
}

#[test]
fn a_running_call_shows_the_output_it_has_produced() {
    let opts = RenderOptions::new(80, false);
    let c = running(
        "bash",
        "bash cargo build",
        "Compiling pacode-tui\nFinished dev\n",
    );
    let text = text_of(&render(&c, 80, &opts, 0, false));
    let body = body_rows(&text, &opts);

    // The bytes received so far — not zero of them, and not the finished
    // cell's all-at-once body. The first row is the `$` input line.
    assert_eq!(body.len(), 4, "{body:?}");
    assert!(body[1].contains("Compiling pacode-tui"), "{body:?}");
    assert!(body[2].contains("Finished dev"), "{body:?}");
}

#[test]
fn a_live_body_marks_its_last_line_as_still_arriving() {
    let opts = RenderOptions::new(80, false);
    let out = "first\nsecond line of output";
    let live_text = text_of(&render(
        &running("read", "read x.rs", out),
        80,
        &opts,
        0,
        false,
    ));
    let live = body_rows(&live_text, &opts);
    let done_text = text_of(&render(
        &call("read", "read x.rs", ToolStatus::Ok, out),
        80,
        &opts,
        0,
        false,
    ));
    let done = body_rows(&done_text, &opts);

    let cursor = stream::live_cursor(&opts);
    let live_tail = live.last().copied().unwrap_or_default();
    let done_tail = done.last().copied().unwrap_or_default();
    assert!(done_tail.contains("second line of output"), "{done_tail:?}");
    assert!(
        live_tail.ends_with(cursor),
        "the live tail is marked: {live_tail:?}"
    );
    assert_ne!(live_tail, done_tail);
    assert!(
        !done_tail.ends_with(cursor),
        "a finished body must not look live"
    );
}

#[test]
fn a_trailing_newline_puts_the_cursor_on_the_line_being_written() {
    let opts = RenderOptions::new(80, false);
    let c = running("bash", "bash make", "line one\n");
    let text = text_of(&render(&c, 80, &opts, 0, false));
    let body = body_rows(&text, &opts);
    let g = format!(" {} ", opts.glyphs.vline);

    // "line one" is done; the cursor stands on the empty line after it, not
    // on the row above.
    assert_eq!(body.len(), 3, "{body:?}");
    assert!(body[1].contains("line one"), "{body:?}");
    assert_eq!(body[2], format!("{g}{}", stream::live_cursor(&opts)));
}

#[test]
fn the_streaming_cap_is_shown_on_the_cell() {
    let opts = RenderOptions::new(80, false);
    let c = running_capped("tail of the log\n", 3_200);
    let text = text_of(&render(&c, 80, &opts, 0, false));
    let body = body_rows(&text, &opts);
    let cap = crate::state::transcript::MAX_TOOL_STREAM_CHARS;

    // A silently shortened body is indistinguishable from the whole of it.
    assert!(
        body[1].contains(&format!("3200 characters above the {cap}-character")),
        "{body:?}"
    );
    assert!(body[2].contains("tail of the log"), "{body:?}");
}

#[test]
fn ctrl_o_expands_a_live_body_the_same_way_it_expands_a_finished_one() {
    let opts = RenderOptions::new(80, false);
    let log = lines_preview(20);
    // The rows come back owned: they borrow the rendered text, and a closure
    // cannot hand a borrow of its own local back to the caller.
    let cells = |expanded: bool| {
        let text = text_of(&render(
            &running("bash", "bash make", &log),
            80,
            &opts,
            0,
            expanded,
        ));
        body_rows(&text, &opts)
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<String>>()
    };
    let (collapsed, expanded) = (cells(false), cells(true));

    let hidden = 20 - COLLAPSED_HEAD_LINES - COLLAPSED_TAIL_LINES;
    assert!(
        collapsed
            .iter()
            .any(|l| l.contains(&rtext::more_lines(hidden, &opts))),
        "{collapsed:?}"
    );
    assert!(
        !expanded
            .iter()
            .any(|l| l.contains(&rtext::more_lines(hidden, &opts))),
        "{expanded:?}"
    );
    assert!(
        expanded.len() > collapsed.len(),
        "{} vs {}",
        expanded.len(),
        collapsed.len()
    );
    assert!(
        expanded.iter().any(|l| l.contains("output line 19")),
        "{expanded:?}"
    );
}
