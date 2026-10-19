use std::time::Instant;

use pacode_types::{ExplainLevel, RiskLevel, ToastLevel};

use super::plain::line_text;
use super::*;
use crate::state::selection::{MAX_SELECTION_BYTES, MAX_SELECTION_LINES};
use crate::state::transcript::PermExplain;

fn whole() -> super::PlainWindow {
    super::PlainWindow {
        lines: None,
        max_bytes: MAX_SELECTION_BYTES,
    }
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
    let lines = super::render_item_inner(&question_cell(None), None, None, None, &opts, 0, false);
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
    let lines = super::render_item_inner(&answered, None, None, None, &opts, 0, false);
    let joined: String = lines
        .iter()
        .map(|l| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(joined.contains("Telegram Bot"), "{joined}");
    assert!(joined.contains("Multi-Interface"), "{joined}");
    assert!(!joined.contains("waiting for your answer"), "{joined}");

    let dismissed = question_cell(Some(pacode_types::QuestionAnswer::cancelled()));
    let lines = super::render_item_inner(&dismissed, None, None, None, &opts, 0, false);
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
    let lines = super::render_item_inner(&answered, None, None, None, &opts, 0, false);
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
    super::render_item_inner(&kind, None, None, None, &opts, 0, false)
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
    assert!(joined.contains(" ✗  turn failed · auth"), "{joined}");
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
    let gutter_lines = joined.lines().filter(|l| l.starts_with(" │ ")).count();
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

#[test]
fn plain_lines_stop_at_the_selection_hard_cap() {
    let opts = pacode_render::RenderOptions::new(20, false);
    let mut t = Transcript::new(MAX_SELECTION_LINES + 1);
    for id in 0..=MAX_SELECTION_LINES as u64 {
        t.cells.push_back(Cell {
            id,
            kind: CellKind::Gap,
            version: 0,
            ts_ms: id,
            stats: None,
            perm_explain: None,
        });
    }

    let lines = super::plain_lines(&mut t, 20, &opts, 0, 0, whole());
    assert_eq!(lines.lines.len(), MAX_SELECTION_LINES);
}

fn copy_all(t: &mut Transcript, width: u16, opts: &pacode_render::RenderOptions) -> String {
    use crate::state::selection::{Point, Selection};
    let lines = super::plain_lines(t, width, opts, 0, 0, whole());
    let last = lines.first_line + lines.lines.len().saturating_sub(1);
    let selection = Selection {
        anchor: Point { line: 0, col: 0 },
        head: Point {
            line: last,
            col: width - 1,
        },
        active: true,
        ..Selection::default()
    };
    crate::ui::extract_selection_text(&lines, &selection, width)
}

/// Decoration is drawn, never copied: a selection across a wrapped answer, a
/// user band and a tool body yields the text alone — no `●`, no indent, no
/// `▌`, no ` │ ` gutter.
#[test]
fn a_copy_drops_markers_bars_and_gutters() {
    let opts = pacode_render::RenderOptions::new(30, false);
    let mut t = Transcript::new(16);
    let item = |seq, kind| pacode_types::TranscriptItem {
        seq,
        agent: pacode_types::AgentId::main(),
        ts_ms: seq,
        kind,
    };
    t.upsert(
        item(
            1,
            TranscriptKind::User {
                text: "make me a weather bot".into(),
            },
        ),
        Instant::now(),
    );
    t.upsert(
        item(
            2,
            TranscriptKind::Assistant {
                text: "Sure, I will scaffold the command line first.".into(),
                complete: true,
            },
        ),
        Instant::now(),
    );
    t.upsert(
        item(
            3,
            TranscriptKind::ToolCall {
                call_id: "call_1".into(),
                name: "read".into(),
                title: "read src/main.rs".into(),
                intent: None,
                status: pacode_types::transcript::ToolStatus::Ok,
                preview: "fn main() {}\nmore".into(),
                diff: None,
                duration_ms: None,
                task: None,
            },
        ),
        Instant::now(),
    );
    t.flush_stream();
    let copied = copy_all(&mut t, 30, &opts);
    let rows: Vec<&str> = copied.lines().collect();
    assert!(rows.contains(&"make me a weather bot"), "{copied}");
    // The answer wraps at 30 columns; both rows lose the marker and indent.
    assert!(rows.contains(&"Sure, I will scaffold the"), "{copied}");
    assert!(rows.contains(&"command line first."), "{copied}");
    assert!(rows.contains(&"fn main() {}"), "{copied}");
    assert!(rows.contains(&"more"), "{copied}");
    for bad in ["▌", "●", "│"] {
        assert!(!copied.contains(bad), "{bad} copied: {copied}");
    }
}

#[test]
fn live_stable_prefix_cache_survives_tail_growth() {
    let opts = pacode_render::RenderOptions::new(80, false);
    let mut t = Transcript::new(16);
    t.upsert(
        pacode_types::TranscriptItem {
            seq: 1,
            agent: pacode_types::AgentId::main(),
            ts_ms: 0,
            kind: TranscriptKind::Assistant {
                text: "first\n\nsecond".into(),
                complete: false,
            },
        },
        Instant::now(),
    );
    t.flush_stream();
    let first = super::plain_lines(&mut t, 80, &opts, 0, 0, whole());
    let lines = t.cache.total_lines();

    t.push_delta(1, " tail", false, Instant::now());
    t.flush_stream();
    let second = super::plain_lines(&mut t, 80, &opts, 1, 0, whole());

    assert_ne!(first.lines, second.lines);
    assert_eq!(t.cache.total_lines(), lines);
}

#[test]
fn a_second_render_at_the_same_width_is_served_from_the_cache() {
    let opts = pacode_render::RenderOptions::new(80, false);
    let mut t = Transcript::new(16);
    for id in 0..4 {
        t.cells.push_back(Cell {
            id,
            kind: CellKind::Item(TranscriptKind::User {
                text: format!("message {id}"),
            }),
            version: 0,
            ts_ms: id,
            stats: None,
            perm_explain: None,
        });
    }

    let first = super::plain_lines(&mut t, 80, &opts, 0, 0, whole());
    let cached_after_first = t.cache.total_lines();
    assert!(cached_after_first > 0, "the first render fills the cache");

    // A fresh anim frame (a spinner tick) must not re-render anything: the
    // cache contents are byte-identical and nothing new lands in it.
    let second = super::plain_lines(&mut t, 80, &opts, 7, 0, whole());
    assert_eq!(first.lines, second.lines, "a cached render is identical");
    assert_eq!(
        t.cache.total_lines(),
        cached_after_first,
        "the second render found every cell in the cache"
    );
}

fn permission_cell(risk: Option<RiskLevel>, explain: Option<PermExplain>) -> Cell {
    Cell {
        id: 1,
        kind: CellKind::Item(TranscriptKind::Permission(
            pacode_types::PermissionRequest {
                id: pacode_types::PermissionId::new("perm_1"),
                agent: pacode_types::AgentId::main(),
                agent_name: "main".into(),
                call_id: pacode_types::CallId::new("call_1"),
                tool: "bash".into(),
                title: "Bash: rm -rf /tmp/x".into(),
                detail: "rm -rf /tmp/x".into(),
                risk,
                created_at_ms: 1,
            },
        )),
        version: 0,
        ts_ms: 0,
        stats: None,
        perm_explain: explain,
    }
}

fn cell_text(cell: &Cell, width: u16) -> String {
    let opts = pacode_render::RenderOptions::new(width, false);
    super::render_cell_for_test(cell, &opts)
        .iter()
        .map(|l| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() })
        .collect::<Vec<_>>()
        .join("\n")
}
fn tool_item(seq: u64, status: ToolStatus, preview: &str) -> pacode_types::TranscriptItem {
    pacode_types::TranscriptItem {
        seq,
        agent: pacode_types::AgentId::main(),
        ts_ms: 0,
        kind: TranscriptKind::ToolCall {
            call_id: pacode_types::CallId::new("call_1"),
            name: "bash".into(),
            title: "bash cargo build".into(),
            intent: None,
            status,
            preview: preview.to_string(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    }
}

#[test]
fn pressing_e_marks_the_prompt_until_the_answer_lands() {
    let cell = permission_cell(Some(RiskLevel::Confirm), Some(PermExplain::Requested));
    let joined = cell_text(&cell, 80);
    assert!(joined.contains("explain requested"), "{joined}");
    // The verdict keys stay visible under the pending mark.
    assert!(joined.contains("y allow"), "{joined}");
}

#[test]
fn a_landed_explanation_renders_level_and_text_and_keeps_the_keys() {
    let cell = permission_cell(
        None,
        Some(PermExplain::Done {
            text: "recursively deletes the tmp dir".into(),
            level: Some(ExplainLevel::High),
        }),
    );
    let joined = cell_text(&cell, 80);
    assert!(joined.contains("explain · high:"), "{joined}");
    assert!(
        joined.contains("recursively deletes the tmp dir"),
        "{joined}"
    );
    assert!(joined.contains("y allow"), "{joined}");
    // No risk on the request -> no chip.
    assert!(!joined.contains("risk:"), "{joined}");
}

#[test]
fn an_honest_explain_failure_has_no_rating_word() {
    let cell = permission_cell(
        Some(RiskLevel::Catastrophic),
        Some(PermExplain::Done {
            text: "explain unavailable under zero-data-retention".into(),
            level: None,
        }),
    );
    let joined = cell_text(&cell, 80);
    assert!(joined.contains("risk: catastrophic"), "{joined}");
    assert!(joined.contains("explain:"), "{joined}");
    assert!(
        joined.contains("explain unavailable under zero-data-retention"),
        "{joined}"
    );
}

#[test]
fn a_copy_materializes_only_the_window_it_reads() {
    let opts = pacode_render::RenderOptions::new(40, false);
    let mut t = Transcript::new(4_000);
    let now = Instant::now();
    for seq in 0..500 {
        t.upsert(
            pacode_types::TranscriptItem {
                seq,
                agent: pacode_types::AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: format!("row {seq}"),
                },
            },
            now,
        );
    }

    let whole = super::plain_lines(&mut t, 40, &opts, 0, 0, whole());
    assert!(whole.lines.len() >= 1_000, "{}", whole.lines.len());

    let tail = super::plain_lines(
        &mut t,
        40,
        &opts,
        0,
        0,
        super::PlainWindow::for_lines(whole.lines.len() - 3..=whole.lines.len() - 1),
    );
    assert_eq!(tail.first_line, whole.lines.len() - 3);
    assert!(tail.lines.len() <= 4, "{}", tail.lines.len());
    assert!(tail.lines.iter().all(|line| !line.contains("row 0 ")));

    // A window that starts inside a row still reports where its first line
    // stands, or the copy would slice the wrong text.
    let inside = super::plain_lines(
        &mut t,
        40,
        &opts,
        0,
        0,
        super::PlainWindow::for_lines(5..=7),
    );
    assert_eq!(inside.first_line, 5);
    assert_eq!(inside.lines.len(), 3);
    assert_eq!(inside.lines, whole.lines[5..8]);
}

#[test]
fn a_copy_stops_at_the_byte_cap_rather_than_growing_without_bound() {
    let opts = pacode_render::RenderOptions::new(200, false);
    let mut t = Transcript::new(64);
    let now = Instant::now();
    for seq in 0..8 {
        t.upsert(
            pacode_types::TranscriptItem {
                seq,
                agent: pacode_types::AgentId::main(),
                ts_ms: seq,
                kind: TranscriptKind::User {
                    text: "z".repeat(200_000),
                },
            },
            now,
        );
    }

    let lines = super::plain_lines(
        &mut t,
        200,
        &opts,
        0,
        0,
        super::PlainWindow {
            lines: None,
            max_bytes: 64 * 1024,
        },
    );
    let bytes: usize = lines.lines.iter().map(String::len).sum();
    assert!(bytes <= 64 * 1024 + 1_000, "{bytes}");
    assert!(!lines.lines.is_empty());
}

fn prepared_text(t: &mut Transcript, opts: &pacode_render::RenderOptions) -> String {
    let area = Rect::new(0, 0, 80, 40);
    super::prepare(area, t, opts, 0, 0)
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_running_tool_body_is_never_served_from_the_line_cache() {
    let opts = pacode_render::RenderOptions::new(80, false);
    let mut t = Transcript::new(16);
    t.upsert(tool_item(1, ToolStatus::Running, ""), Instant::now());

    // Nothing has been produced yet: a header and an input row, and not one
    // line of it in the cache — the running cell renders fresh every frame.
    let first = prepared_text(&mut t, &opts);
    assert!(!first.contains("Compiling"), "{first}");
    assert_eq!(t.cache.total_lines(), 0, "a running tool is never cached");

    // The cell itself does not change, only the output behind it.
    t.push_tool_output(1, "Compiling pacode-tui\n");
    let second = prepared_text(&mut t, &opts);
    assert!(second.contains("Compiling pacode-tui"), "{second}");
    assert_eq!(t.cache.total_lines(), 0, "a running tool is never cached");

    t.push_tool_output(1, "Finished dev\n");
    let third = prepared_text(&mut t, &opts);
    assert!(third.contains("Finished dev"), "{third}");
    assert_eq!(t.cache.total_lines(), 0, "a running tool is never cached");
}

#[test]
fn a_finished_tool_body_takes_the_cache_again() {
    let opts = pacode_render::RenderOptions::new(80, false);
    let mut t = Transcript::new(16);
    t.upsert(tool_item(1, ToolStatus::Running, ""), Instant::now());
    prepared_text(&mut t, &opts);
    assert_eq!(t.cache.total_lines(), 0);

    t.upsert(
        tool_item(1, ToolStatus::Ok, "Compiling pacode-tui\nFinished dev"),
        Instant::now(),
    );

    let text = prepared_text(&mut t, &opts);
    assert!(text.contains("Finished dev"), "{text}");
    assert!(
        t.cache.total_lines() > 0,
        "a finished cell is cacheable again"
    );
    // The live bytes are gone with the live cell: the finished preview is the
    // body, and nothing is held twice.
    assert_eq!(t.tool_streams.get(1), None);
}
