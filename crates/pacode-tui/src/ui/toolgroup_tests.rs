//! Which cells fold into an exploration group, and what a group draws.

use pacode_render::{ColorMode, Theme, display_width};
use pacode_types::ToastLevel;

use super::*;

pub(crate) fn cell(id: u64, kind: TranscriptKind) -> Cell {
    Cell {
        id,
        kind: CellKind::Item(kind),
        version: 0,
        ts_ms: id,
        stats: None,
        perm_explain: None,
    }
}

pub(crate) fn tool(id: u64, name: &str, arg: &str, status: ToolStatus, preview: &str) -> Cell {
    cell(
        id,
        TranscriptKind::ToolCall {
            call_id: pacode_types::CallId::new(format!("call_{id}")),
            name: name.into(),
            title: format!("{name} {arg}"),
            intent: None,
            status,
            preview: preview.into(),
            diff: None,
            duration_ms: Some(5),
            task: None,
        },
    )
}

pub(crate) fn read(id: u64, path: &str) -> Cell {
    tool(
        id,
        "read",
        path,
        ToolStatus::Ok,
        "     1\tfn main() {}\n     2\t}",
    )
}

fn assistant(id: u64) -> Cell {
    cell(
        id,
        TranscriptKind::Assistant {
            text: "Looking around.".into(),
            complete: true,
        },
    )
}

fn reasoning(id: u64) -> Cell {
    cell(
        id,
        TranscriptKind::Reasoning {
            text: "hmm".into(),
            complete: true,
        },
    )
}

fn deque(cells: Vec<Cell>) -> VecDeque<Cell> {
    cells.into_iter().collect()
}

fn segs(cells: &VecDeque<Cell>) -> Vec<Segment> {
    segments(cells.iter(), &HashSet::new(), false)
}

fn truecolor(width: u16) -> RenderOptions {
    RenderOptions::with_theme_color(width, false, Theme::truecolor(), ColorMode::Truecolor)
}

fn text(line: &Line<'_>) -> String {
    let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    row.trim_end().to_string()
}

#[test]
fn two_or_more_consecutive_exploration_calls_fold() {
    let cells = deque(vec![
        read(1, "a.rs"),
        tool(2, "grep", "\"todo\"", ToolStatus::Ok, "a.rs:1:todo"),
        tool(3, "glob", "\"*.rs\"", ToolStatus::Ok, "a.rs\nb.rs"),
        tool(4, "ls", "src", ToolStatus::Ok, "a.rs (3 B)"),
        tool(5, "symbols", "src", ToolStatus::Ok, "fn main 1-2"),
    ]);
    assert_eq!(segs(&cells), [Segment::Group(0..5)]);
}

#[test]
fn a_lone_exploration_call_stays_a_cell() {
    let cells = deque(vec![assistant(1), read(2, "a.rs"), assistant(3)]);
    assert_eq!(
        segs(&cells),
        [Segment::Cell(0), Segment::Cell(1), Segment::Cell(2)]
    );
}

#[test]
fn anything_drawn_between_two_calls_breaks_the_run() {
    let breakers = [
        assistant(9),
        tool(9, "bash", "ls", ToolStatus::Ok, "x"),
        tool(9, "edit", "a.rs", ToolStatus::Ok, "x"),
        cell(
            9,
            TranscriptKind::Notice {
                level: ToastLevel::Info,
                text: "note".into(),
            },
        ),
        cell(9, TranscriptKind::User { text: "hi".into() }),
    ];
    for breaker in breakers {
        let cells = deque(vec![read(1, "a.rs"), breaker.clone(), read(2, "b.rs")]);
        assert_eq!(
            segs(&cells),
            [Segment::Cell(0), Segment::Cell(1), Segment::Cell(2)],
            "{:?}",
            breaker.kind
        );
    }
    let mut gap = read(9, "x");
    gap.kind = CellKind::Gap;
    let cells = deque(vec![read(1, "a.rs"), gap, read(2, "b.rs")]);
    assert_eq!(segs(&cells).len(), 3);
}

#[test]
fn a_failed_or_refused_call_is_not_absorbed() {
    for status in [ToolStatus::Error, ToolStatus::Denied] {
        let cells = deque(vec![
            read(1, "a.rs"),
            read(2, "b.rs"),
            tool(3, "read", "gone.rs", status, "no such file"),
            read(4, "c.rs"),
            read(5, "d.rs"),
        ]);
        assert_eq!(
            segs(&cells),
            [Segment::Group(0..2), Segment::Cell(2), Segment::Group(3..5)],
            "{status:?}"
        );
    }
}

#[test]
fn a_running_call_can_close_a_group() {
    let cells = deque(vec![
        read(1, "a.rs"),
        tool(2, "read", "b.rs", ToolStatus::Running, ""),
    ]);
    assert_eq!(segs(&cells), [Segment::Group(0..2)]);
    assert!(has_running(&cells, 0..2));
}

#[test]
fn an_expanded_call_breaks_out_and_the_rest_fold_around_it() {
    let cells = deque((1..=5).map(|id| read(id, "x.rs")).collect());
    let expanded: HashSet<u64> = [3].into();
    assert_eq!(
        segments(cells.iter(), &expanded, false),
        [Segment::Group(0..2), Segment::Cell(2), Segment::Group(3..5)]
    );
}

#[test]
fn hidden_reasoning_is_invisible_to_a_run_but_shown_reasoning_breaks_it() {
    let cells = deque(vec![
        read(1, "a.rs"),
        reasoning(2),
        read(3, "b.rs"),
        reasoning(4),
    ]);
    // Thinking hidden: the reasoning between folds in, the trailing one stays
    // out (it closes no member).
    assert_eq!(
        segments(cells.iter(), &HashSet::new(), false),
        [Segment::Group(0..3), Segment::Cell(3)]
    );
    assert_eq!(member_ids(&cells, 0..3), [1, 3]);
    // Thinking shown: the reasoning row is drawn, so it is in the way.
    assert_eq!(segments(cells.iter(), &HashSet::new(), true).len(), 4);
}

#[test]
fn the_run_around_a_cell_ignores_expansion() {
    let cells = deque(vec![
        assistant(1),
        read(2, "a.rs"),
        read(3, "b.rs"),
        read(4, "c.rs"),
    ]);
    assert_eq!(run_around(&cells, 2, false), Some(1..4));
    assert_eq!(run_around(&cells, 0, false), None);
}

#[test]
fn a_group_draws_a_tally_header_and_one_row_per_call() {
    let opts = truecolor(70);
    let cells = deque(vec![
        read(1, "crates/pacode-tui/src/ui/input.rs"),
        tool(
            2,
            "grep",
            "\"prompt_frame\"",
            ToolStatus::Ok,
            "a.rs:1:x\nb.rs:2:y\nc.rs:3:z\nd.rs:4:w",
        ),
        tool(3, "ls", "src", ToolStatus::Ok, "a/\nb/\nc.rs (1 B)"),
        read(4, "b.rs"),
    ]);
    let lines = render(&cells, 0..4, &opts, 0, &LineCache::new(64));
    let rows: Vec<String> = lines.iter().map(text).collect();
    assert_eq!(rows.len(), 6, "{rows:#?}");
    assert!(rows[0].contains("EXPLORE"), "{}", rows[0]);
    assert!(
        rows[0].contains("2 reads · 1 search · 1 list"),
        "{}",
        rows[0]
    );
    let g = format!(" {} ", opts.glyphs.vline);
    assert_eq!(
        rows[1],
        format!("{g}Read crates/pacode-tui/src/ui/input.rs → 2 lines")
    );
    assert_eq!(rows[2], format!("{g}Search \"prompt_frame\" → 4 matches"));
    assert_eq!(rows[3], format!("{g}List src → 3 entries"));
    assert_eq!(rows[4], format!("{g}Read b.rs → 2 lines"));
    assert_eq!(lines[5], Line::default(), "the gap closes the group");
}

#[test]
fn a_lone_search_row_shows_the_hit_or_the_miss_itself() {
    let opts = truecolor(70);
    let cells = deque(vec![
        tool(1, "grep", "\"nope\"", ToolStatus::Ok, "no hits"),
        tool(2, "glob", "\"*.toml\"", ToolStatus::Ok, "Cargo.toml"),
    ]);
    let rows: Vec<String> = render(&cells, 0..2, &opts, 0, &LineCache::new(64))
        .iter()
        .map(text)
        .collect();
    assert!(rows[1].ends_with("→ no hits"), "{}", rows[1]);
    assert!(rows[2].ends_with("→ Cargo.toml"), "{}", rows[2]);
}

#[test]
fn a_capped_preview_counts_as_at_least() {
    let opts = truecolor(70);
    let preview = format!(
        "{}\n\n[... 900 characters truncated ...]\n\nlast\n— 40 more lines; read with offset=200",
        (1..=3)
            .map(|i| format!("{i:>6}\tx"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let cells = deque(vec![
        tool(1, "read", "big.rs", ToolStatus::Ok, &preview),
        read(2, "b.rs"),
    ]);
    let rows: Vec<String> = render(&cells, 0..2, &opts, 0, &LineCache::new(64))
        .iter()
        .map(text)
        .collect();
    assert!(rows[1].ends_with("→ 4+ lines"), "{}", rows[1]);
}

#[test]
fn a_running_group_is_pending_and_its_last_row_runs() {
    let opts = truecolor(60);
    let cells = deque(vec![
        read(1, "a.rs"),
        tool(2, "read", "layout.rs", ToolStatus::Running, ""),
    ]);
    let lines = render(&cells, 0..2, &opts, 0, &LineCache::new(64));
    assert!(text(&lines[0]).contains("running"), "{}", text(&lines[0]));
    assert!(
        text(&lines[2]).contains("Read layout.rs running"),
        "{}",
        text(&lines[2])
    );
    let (gap, block) = lines.split_last().expect("a block and its gap");
    assert_eq!(*gap, Line::default());
    for row in block {
        assert_eq!(
            row.spans
                .iter()
                .map(|s| display_width(&s.content))
                .sum::<usize>(),
            60
        );
        assert_eq!(
            row.spans.last().and_then(|s| s.style.bg),
            opts.theme.tool_pending_bg.bg
        );
    }
    let done = deque(vec![read(1, "a.rs"), read(2, "b.rs")]);
    let lines = render(&done, 0..2, &opts, 0, &LineCache::new(64));
    for row in &lines[..lines.len() - 1] {
        for span in row.spans.iter().skip(1) {
            assert_eq!(span.style.bg, opts.theme.tool_success_bg.bg);
        }
    }
}

#[test]
fn a_group_has_no_band_under_ansi() {
    let opts = RenderOptions::with_theme_color(60, false, Theme::ansi(), ColorMode::Ansi);
    let cells = deque(vec![read(1, "a.rs"), read(2, "b.rs")]);
    for row in &render(&cells, 0..2, &opts, 0, &LineCache::new(64))[1..] {
        assert!(row.spans.iter().all(|s| s.style.bg.is_none()));
    }
}

#[test]
fn a_long_run_shows_the_newest_calls_and_counts_the_rest() {
    let opts = truecolor(60);
    let cells = deque((1..=11).map(|id| read(id, &format!("f{id}.rs"))).collect());
    let lines = render(&cells, 0..11, &opts, 0, &LineCache::new(64));
    let rows: Vec<String> = lines.iter().map(text).collect();
    // Header, `… 3 more calls`, the newest eight, the gap.
    assert_eq!(rows.len(), 1 + 1 + MAX_GROUP_ROWS + 1, "{rows:#?}");
    assert!(rows[0].contains("11 reads"), "{}", rows[0]);
    assert!(rows[1].ends_with("… 3 more calls"), "{}", rows[1]);
    assert!(rows[2].contains("f4.rs"), "{}", rows[2]);
    assert!(rows[9].contains("f11.rs"), "{}", rows[9]);
    assert!(!rows.iter().any(|r| r.contains("f3.rs")));
}

#[test]
fn every_line_of_a_group_belongs_to_one_of_its_cells() {
    // 11 calls: three hidden, eight shown, one invisible reasoning between.
    let mut cells: Vec<Cell> = (1..=11).map(|id| read(id, "x.rs")).collect();
    cells.insert(5, reasoning(50));
    let cells = deque(cells);
    let mut spans = Vec::new();
    let mut heads = Vec::new();
    // Header + more row + 8 rows + gap = 11 lines, starting at line 4.
    push_spans(&cells, 0..12, 4..15, &mut spans, &mut heads);
    assert_eq!(
        spans,
        [
            (1, 4, 4),
            (2, 4, 4),
            (3, 4, 4),
            (4, 4, 7),
            (5, 7, 8),
            (50, 8, 8),
            (6, 8, 9),
            (7, 9, 10),
            (8, 10, 11),
            (9, 11, 12),
            (10, 12, 13),
            (11, 13, 15),
        ]
    );
    assert_eq!(
        heads,
        [GroupHead {
            lines: 4..6,
            first: 1,
            last: 11
        }]
    );
}

#[test]
fn a_group_key_changes_with_every_member() {
    let mut cells = deque(vec![read(1, "a.rs"), read(2, "b.rs")]);
    let before = cache_key(&cells, 0..2, 80, 0);
    assert_eq!(before, cache_key(&cells, 0..2, 80, 0));
    cells[1].version += 1;
    assert_ne!(before, cache_key(&cells, 0..2, 80, 0));
    cells.push_back(read(3, "c.rs"));
    assert_ne!(
        cache_key(&cells, 0..2, 80, 0),
        cache_key(&cells, 0..3, 80, 0)
    );
    // Never one of the members' own keys.
    assert_ne!(
        before,
        CacheKey::cell(1, 0, 80, false, 0),
        "a group cannot collide with its first cell"
    );
}

#[test]
fn only_the_finished_shown_calls_rows_are_memoized() {
    let opts = truecolor(60);
    // Eleven calls: three hidden behind `… more`, the newest one running.
    let mut cells: Vec<Cell> = (1..=10).map(|id| read(id, "x.rs")).collect();
    cells.push(tool(11, "read", "y.rs", ToolStatus::Running, ""));
    let cells = deque(cells);
    let mut cache = LineCache::new(1_000);
    warm_rows(&cells, 0..11, &opts, &mut cache);
    let memo = |id: u64| CacheKey::row(id, 0, opts.width, opts.cache_epoch);
    for id in 1..=3 {
        assert!(cache.peek(memo(id)).is_none(), "hidden call {id}");
    }
    for id in 4..=10 {
        assert!(cache.peek(memo(id)).is_some(), "shown finished call {id}");
    }
    assert!(
        cache.peek(memo(11)).is_none(),
        "a running row is never memoized"
    );
    assert_eq!(cache.total_lines(), 7);

    // Drawn from the memo, the group is the same as drawn from scratch.
    let fresh = render(&cells, 0..11, &opts, 0, &LineCache::new(1));
    assert_eq!(render(&cells, 0..11, &opts, 0, &cache), fresh);
}

#[test]
fn a_row_outcome_drops_ansi_escapes() {
    let opts = truecolor(70);
    let cells = deque(vec![
        tool(
            1,
            "grep",
            "\"error\"",
            ToolStatus::Ok,
            "\u{1b}[31merror\u{1b}[0m",
        ),
        read(2, "b.rs"),
    ]);
    let rows: Vec<String> = render(&cells, 0..2, &opts, 0, &LineCache::new(64))
        .iter()
        .map(text)
        .collect();
    assert!(rows[1].ends_with("→ error"), "{:?}", rows[1]);
}
