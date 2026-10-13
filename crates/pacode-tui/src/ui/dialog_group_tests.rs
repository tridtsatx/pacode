//! Exploration groups in the transcript view: what the dialog draws, which
//! line belongs to which call, how clicks and `ctrl+o` open and fold a group,
//! what a copy holds, and when a group comes from the cache.

use std::time::Instant;

use pacode_types::transcript::ToolStatus;
use pacode_types::{AgentId, CallId, TranscriptItem};

use super::plain::line_text;
use super::*;
use crate::state::selection::MAX_SELECTION_BYTES;

fn opts(width: u16) -> RenderOptions {
    RenderOptions::new(width, false)
}

fn tool_item(seq: u64, name: &str, arg: &str, status: ToolStatus, preview: &str) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: seq,
        kind: TranscriptKind::ToolCall {
            call_id: CallId::new(format!("call_{seq}")),
            name: name.into(),
            title: format!("{name} {arg}"),
            intent: None,
            status,
            preview: preview.into(),
            diff: None,
            duration_ms: Some(3),
            task: None,
        },
    }
}

fn read_item(seq: u64, path: &str) -> TranscriptItem {
    tool_item(
        seq,
        "read",
        path,
        ToolStatus::Ok,
        "     1\tfn a() {}\n     2\t}",
    )
}

fn answer_item(seq: u64, text: &str) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: seq,
        kind: TranscriptKind::Assistant {
            text: text.into(),
            complete: true,
        },
    }
}

fn with_reads(n: u64) -> Transcript {
    let mut t = Transcript::new(64);
    t.upsert(answer_item(1, "Looking around."), Instant::now());
    for seq in 2..2 + n {
        t.upsert(read_item(seq, &format!("f{seq}.rs")), Instant::now());
    }
    t
}

/// The whole prepared view, padding trimmed.
fn view(t: &mut Transcript, width: u16, height: u16) -> Vec<String> {
    prepare(Rect::new(0, 0, width, height), t, &opts(width), 0, 0)
        .lines
        .iter()
        .map(|l| line_text(l).trim_end().to_string())
        .collect()
}

fn count(rows: &[String], needle: &str) -> usize {
    rows.iter().filter(|r| r.contains(needle)).count()
}

#[test]
fn consecutive_reads_draw_as_one_group() {
    let mut t = with_reads(3);
    let rows = view(&mut t, 80, 40);
    assert_eq!(count(&rows, "EXPLORE"), 1, "{rows:#?}");
    assert_eq!(
        count(&rows, "READ"),
        0,
        "no call draws its own cell: {rows:#?}"
    );
    assert_eq!(count(&rows, "→ 2 lines"), 3, "{rows:#?}");
    // Every call still owns a line range, and the header is a click target.
    for id in 2..5 {
        assert!(t.cell_line_index.contains_key(&id), "{id}");
    }
    assert_eq!(t.group_heads.len(), 1);
}

#[test]
fn clicking_the_header_opens_every_call_and_collapsing_one_folds_them_back() {
    let mut t = with_reads(3);
    view(&mut t, 80, 40);
    let head = t.group_heads[0].lines.start;
    assert!(t.toggle_at_line(head));
    assert!((2..5).all(|id| t.is_expanded(id)));
    let rows = view(&mut t, 80, 40);
    assert_eq!(count(&rows, "EXPLORE"), 0, "{rows:#?}");
    assert_eq!(
        count(&rows, "READ"),
        3,
        "each call is its own full cell: {rows:#?}"
    );
    assert!(t.group_heads.is_empty());

    // A click on any of the opened calls folds the whole run back.
    let (_, start, _) = t.cell_lines[t.cell_line_index[&3]];
    assert!(t.toggle_at_line(start));
    assert!((2..5).all(|id| !t.is_expanded(id)));
    let rows = view(&mut t, 80, 40);
    assert_eq!(count(&rows, "EXPLORE"), 1, "{rows:#?}");
}

#[test]
fn clicking_one_call_row_opens_that_call_alone() {
    let mut t = with_reads(5);
    view(&mut t, 80, 60);
    // The middle call's own row: its span ends right after it.
    let (_, _, end) = t.cell_lines[t.cell_line_index[&4]];
    assert!(t.toggle_at_line(end - 1));
    assert!(t.is_expanded(4));
    assert!([2, 3, 5, 6].iter().all(|id| !t.is_expanded(*id)));
    let rows = view(&mut t, 80, 60);
    // Two groups of two fold around the opened call.
    assert_eq!(count(&rows, "EXPLORE"), 2, "{rows:#?}");
    assert_eq!(count(&rows, "READ"), 1, "{rows:#?}");
    assert!(
        rows.iter()
            .any(|r| r.contains("f4.rs") && r.contains("READ"))
    );
}

#[test]
fn ctrl_o_opens_the_newest_group_and_folds_it_back() {
    let mut t = with_reads(3);
    view(&mut t, 80, 40);
    assert!(t.toggle_last_expandable());
    assert!((2..5).all(|id| t.is_expanded(id)));
    assert!(t.toggle_last_expandable());
    assert!((2..5).all(|id| !t.is_expanded(id)));
    let rows = view(&mut t, 80, 40);
    assert_eq!(count(&rows, "EXPLORE"), 1, "{rows:#?}");
}

#[test]
fn a_failed_call_ends_the_group_and_keeps_its_own_cell() {
    let mut t = with_reads(2);
    t.upsert(
        tool_item(9, "read", "gone.rs", ToolStatus::Error, "no such file"),
        Instant::now(),
    );
    let rows = view(&mut t, 80, 40);
    assert_eq!(count(&rows, "EXPLORE"), 1, "{rows:#?}");
    assert_eq!(
        count(&rows, "READ"),
        1,
        "the failure stays visible: {rows:#?}"
    );
    assert!(rows.iter().any(|r| r.contains("no such file")), "{rows:#?}");
}

#[test]
fn a_copy_of_a_group_drops_the_gutter() {
    let mut t = with_reads(2);
    let opts = opts(60);
    let lines = plain_lines(
        &mut t,
        60,
        &opts,
        0,
        0,
        PlainWindow {
            lines: None,
            max_bytes: MAX_SELECTION_BYTES,
        },
    );
    let copied: Vec<String> = lines
        .lines
        .iter()
        .zip(&lines.leads)
        .map(|(text, lead)| {
            text.chars()
                .skip(*lead)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    assert!(
        copied.contains(&"Read f2.rs → 2 lines".to_string()),
        "{copied:#?}"
    );
    assert!(
        copied.contains(&"Read f3.rs → 2 lines".to_string()),
        "{copied:#?}"
    );
    assert!(
        copied.iter().any(|row| row.contains("EXPLORE")),
        "{copied:#?}"
    );
    assert!(!copied.iter().any(|row| row.contains(opts.glyphs.vline)));
}

#[test]
fn a_running_group_renders_fresh_and_a_finished_one_is_cached() {
    let mut t = Transcript::new(16);
    t.upsert(read_item(1, "a.rs"), Instant::now());
    t.upsert(
        tool_item(2, "read", "b.rs", ToolStatus::Running, ""),
        Instant::now(),
    );
    let rows = view(&mut t, 80, 20);
    assert!(
        rows.iter().any(|r| r.contains("Read b.rs running")),
        "{rows:#?}"
    );
    assert_eq!(
        t.cache.total_lines(),
        1,
        "the running group's block is never cached — only the finished call's row"
    );

    // The member finishes: the same group, now cached, with its outcome.
    t.upsert(read_item(2, "b.rs"), Instant::now());
    let rows = view(&mut t, 80, 20);
    assert!(
        rows.iter().any(|r| r.contains("Read b.rs → 2 lines")),
        "{rows:#?}"
    );
    let cached = t.cache.total_lines();
    assert!(cached > 0, "a finished group is cacheable");

    // A new member is a new group block, never the stale one.
    t.upsert(read_item(3, "c.rs"), Instant::now());
    let rows = view(&mut t, 80, 20);
    assert!(rows.iter().any(|r| r.contains("3 reads")), "{rows:#?}");
    assert!(rows.iter().any(|r| r.contains("Read c.rs")), "{rows:#?}");
}

#[test]
fn a_view_scrolled_up_holds_its_place_while_a_group_below_grows() {
    let mut t = Transcript::new(256);
    for seq in 1..=30 {
        t.upsert(
            answer_item(seq, &format!("answer number {seq}")),
            Instant::now(),
        );
    }
    t.upsert(read_item(100, "a.rs"), Instant::now());
    t.upsert(read_item(101, "b.rs"), Instant::now());
    view(&mut t, 60, 10);
    t.scroll_by(20);
    let before = view(&mut t, 60, 10);
    t.upsert(read_item(102, "c.rs"), Instant::now());
    let after = view(&mut t, 60, 10);
    // The bottom row is the "↓ more below" note, which counts the growth.
    assert_eq!(before[..9], after[..9]);
}

/// Outcomes counted out of previews on this thread so far.
fn outcomes_counted() -> usize {
    crate::ui::toolcell::OUTCOMES_COUNTED.with(std::cell::Cell::get)
}

/// A running group redraws every frame, but a finished call never changes:
/// its row is counted out of its preview once, not once per frame.
#[test]
fn a_running_group_never_recounts_its_finished_calls() {
    let mut t = Transcript::new(16);
    t.upsert(read_item(1, "a.rs"), Instant::now());
    t.upsert(read_item(2, "b.rs"), Instant::now());
    t.upsert(
        tool_item(3, "read", "c.rs", ToolStatus::Running, ""),
        Instant::now(),
    );
    let opts = opts(80);
    let area = Rect::new(0, 0, 80, 20);

    let before = outcomes_counted();
    prepare(area, &mut t, &opts, 0, 0);
    assert_eq!(outcomes_counted() - before, 2, "each finished call, once");

    // Frame after frame — the spinner turns, the finished rows do not move.
    let before = outcomes_counted();
    for frame in 1..=10 {
        let rows: Vec<String> = prepare(area, &mut t, &opts, frame, 0)
            .lines
            .iter()
            .map(line_text)
            .collect();
        assert!(rows.iter().any(|r| r.contains("Read a.rs → 2 lines")));
    }
    assert_eq!(outcomes_counted() - before, 0, "no finished call recounted");

    // The running call finishes: it alone is counted, once.
    t.upsert(read_item(3, "c.rs"), Instant::now());
    let before = outcomes_counted();
    for frame in 11..=15 {
        prepare(area, &mut t, &opts, frame, 0);
    }
    assert_eq!(outcomes_counted() - before, 1);
}
