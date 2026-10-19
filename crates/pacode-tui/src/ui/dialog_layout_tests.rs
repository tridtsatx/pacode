//! The windowed transcript frame against a full render: every block measured
//! (from the extent memo when it can), only the blocks overlapping the
//! viewport rendered, and the rows on screen identical to rendering the whole
//! transcript and slicing the viewport out of it.

use pacode_types::transcript::ToolStatus;
use pacode_types::{CallId, ToastLevel, TranscriptKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::*;
use crate::state::transcript::HeaderInfo;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn words(rng: &mut Rng, n: u64) -> String {
    (0..n)
        .map(|_| "lorem ipsum dolor sit amet ".repeat(1 + rng.below(3) as usize))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool(name: &str, status: ToolStatus, preview: String) -> CellKind {
    CellKind::Item(TranscriptKind::ToolCall {
        call_id: CallId::new(format!("call_{name}")),
        name: name.into(),
        title: format!("{name} src/lib.rs"),
        intent: None,
        status,
        preview,
        diff: None,
        duration_ms: Some(5),
        task: None,
    })
}

/// `running`: whether running calls (which draw anew every frame) may occur.
fn random_kind(rng: &mut Rng, running: bool) -> CellKind {
    let pick = rng.below(if running { 9 } else { 8 });
    let n = 1 + rng.below(5);
    match pick {
        0 => CellKind::Gap,
        1 => CellKind::Item(TranscriptKind::User {
            text: words(rng, n),
        }),
        2 => CellKind::Item(TranscriptKind::Assistant {
            text: format!("# title\n\n{}\n\n- a\n- b", words(rng, n)),
            complete: true,
        }),
        3 => CellKind::Item(TranscriptKind::Notice {
            level: ToastLevel::Info,
            text: words(rng, 1),
        }),
        4 => CellKind::Item(TranscriptKind::BashCommand {
            command: "cargo test".into(),
            output: words(rng, n),
            exit_code: Some(rng.below(2) as i32),
            truncated: false,
        }),
        5 => tool("bash", ToolStatus::Ok, words(rng, n)),
        // Reads fold into exploration groups when two or more are adjacent.
        6 | 7 => tool(
            "read",
            ToolStatus::Ok,
            "     1\tfn a() {}\n     2\t}".into(),
        ),
        _ => tool("bash", ToolStatus::Running, String::new()),
    }
}

fn build(seed: u64, n: usize, header: bool, running: bool) -> Transcript {
    let mut rng = Rng(seed | 1);
    let mut t = Transcript::new(n + 10);
    if header {
        t.set_header(HeaderInfo {
            version: "0.1.0".into(),
            day: 3,
            mascot: crate::ui::mascot::MascotKind::Pacman,
            truecolor: true,
        });
    }
    for i in 0..n {
        let stats = (rng.below(4) == 0).then(|| "1.2s".to_string());
        t.cells.push_back(Cell {
            id: i as u64,
            kind: random_kind(&mut rng, running),
            version: rng.below(3) as u32,
            ts_ms: i as u64,
            stats,
            perm_explain: None,
        });
    }
    // One expanded cell, so expansion is part of the layout.
    if n > 3 {
        t.expanded.insert(3);
    }
    t
}

fn drawn(t: &mut Transcript, w: u16, h: u16, opts: &RenderOptions) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    term.draw(|f| {
        let area = f.area();
        let prepared = prepare(area, t, opts, 7, 10_000);
        draw(f, area, &prepared);
    })
    .expect("draw");
    term.backend().buffer().clone()
}

/// Every line of the transcript: the banner, then every block rendered.
fn all_lines(t: &mut Transcript, w: u16, opts: &RenderOptions) -> Vec<Line<'static>> {
    let Transcript {
        cells,
        expanded,
        cache,
        stable_revisions,
        header,
        live_cell,
        tool_streams,
        images,
        ..
    } = t;
    let mut out = header.as_ref().map_or_else(Vec::new, |info| {
        crate::ui::header::render(info, w, opts, 7, 10_000)
    });
    let src = CellsSource {
        cells,
        expanded,
        stable_revisions,
        live_id: *live_cell,
        tool_streams,
        width: w,
        anim_frame: 7,
    };
    let mut parts = RenderParts { cache, images };
    for segment in toolgroup::segments(cells.iter(), expanded, opts.thinking) {
        out.extend(
            segment_block(&src, &segment, opts, &mut parts)
                .lines()
                .iter()
                .cloned(),
        );
    }
    out
}

/// The old algorithm: render everything, then pick the viewport the windowed
/// frame chose (`first_visible_line`, `scroll_from_bottom`) out of it.
fn reference(
    t: &mut Transcript,
    top: usize,
    below: usize,
    w: u16,
    h: u16,
    opts: &RenderOptions,
) -> (Buffer, usize) {
    let all = all_lines(t, w, opts);
    let total = all.len();
    let mut visible: Vec<Line<'static>> = all.into_iter().skip(top).take(h as usize).collect();
    if below > 0
        && let Some(last) = visible.last_mut()
    {
        let noun = if below == 1 { "line" } else { "lines" };
        *last = Line::from(Span::styled(
            format!("↓ {below} more {noun} below"),
            opts.theme.dim,
        ));
    }
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    term.draw(|f| {
        f.render_widget(Paragraph::new(visible), Rect::new(0, 0, w, h));
    })
    .expect("draw");
    (term.backend().buffer().clone(), total)
}

/// Draw `a` windowed and check it against a full render of its twin `b`.
fn assert_matches(a: &mut Transcript, b: &mut Transcript, w: u16, h: u16, what: &str) {
    let opts = RenderOptions::new(w, false);
    let got = drawn(a, w, h, &opts);
    let (want, total) = reference(
        b,
        a.first_visible_line,
        if total_exceeds(a, h) {
            a.scroll_from_bottom
        } else {
            0
        },
        w,
        h,
        &opts,
    );
    assert_eq!(a.rendered_lines, Some(total), "{what}: total lines");
    assert_eq!(got, want, "{what}");
}

fn total_exceeds(t: &Transcript, h: u16) -> bool {
    t.rendered_lines.unwrap_or(0) > h as usize
}

#[test]
fn windowed_draw_matches_full_render_at_several_widths() {
    for seed in [1u64, 7, 42, 1234, 99_991] {
        for header in [false, true] {
            for w in [12u16, 40, 80, 133] {
                for h in [1u16, 7, 24] {
                    for scroll in [0usize, 1, 5, 40, 100_000] {
                        let mut a = build(seed, 60, header, true);
                        let mut b = build(seed, 60, header, true);
                        // Three frames: the first measures, the next answer
                        // from the extent memo (and the scroll anchor holds).
                        for frame in 0..3 {
                            a.scroll_from_bottom = scroll;
                            assert_matches(
                                &mut a,
                                &mut b,
                                w,
                                h,
                                &format!(
                                    "seed {seed} header {header} w {w} h {h} scroll {scroll} frame {frame}"
                                ),
                            );
                        }
                    }
                }
            }
        }
    }
}

/// A line cache far smaller than the transcript evicts what it planned in
/// the same frame and what the last frame showed: the frame still matches a
/// full render, from the memoized heights.
#[test]
fn a_line_cache_smaller_than_the_transcript_still_draws_the_same_rows() {
    for seed in [3u64, 77, 2024] {
        for scroll in [0usize, 9, 300] {
            let mut a = build(seed, 120, true, false);
            a.cache = LineCache::new(40);
            let mut b = build(seed, 120, true, false);
            for frame in 0..3 {
                a.scroll_from_bottom = scroll;
                assert_matches(
                    &mut a,
                    &mut b,
                    80,
                    20,
                    &format!("seed {seed} scroll {scroll} frame {frame}"),
                );
            }
        }
    }
}

#[test]
fn click_map_is_contiguous_and_covers_every_rendered_line() {
    let opts = RenderOptions::new(60, false);
    for header in [false, true] {
        let mut t = build(5, 40, header, true);
        drawn(&mut t, 60, 10, &opts);
        let mut twin = build(5, 40, header, true);
        let total = all_lines(&mut twin, 60, &opts).len();
        assert_eq!(t.rendered_lines, Some(total));
        assert_eq!(t.cell_lines.len(), t.cells.len());
        let mut cursor = t.cell_lines.first().map(|c| c.1).unwrap_or(0);
        for (id, start, end) in &t.cell_lines {
            assert_eq!(*start, cursor, "cell {id} starts where the last ended");
            cursor = *end;
        }
        assert_eq!(cursor, total);
        let first = t.cell_lines.first().map(|c| c.1).unwrap_or(0);
        assert_eq!(first > 0, header, "only the header precedes the first cell");
    }
}

#[test]
fn width_change_and_cache_clear_remeasure() {
    let mut t = build(9, 30, false, true);
    for w in [30u16, 90, 30] {
        let mut fresh = build(9, 30, false, true);
        assert_matches(&mut t, &mut fresh, w, 12, &format!("width {w}"));
    }
    t.cache.clear();
    let mut fresh = build(9, 30, false, true);
    assert_matches(&mut t, &mut fresh, 30, 12, "after a cache clear");
}

#[test]
fn scrolling_a_huge_transcript_lays_out_only_the_viewport() {
    let n = 10_000;
    let height = 40u16;
    let opts = RenderOptions::new(80, false);
    let mut t = build(77, n, false, false);
    drawn(&mut t, 80, height, &opts);
    let blocks = toolgroup::segments(t.cells.iter(), &t.expanded, opts.thinking).len();
    assert_eq!(
        t.cells_laid_out, blocks,
        "the first frame measures every block once"
    );
    assert_eq!(t.extents.len(), blocks);

    for scroll in [0usize, 1, 37, 500, 5_000, 20_000, 100_000, 3, 0] {
        t.scroll_from_bottom = scroll;
        drawn(&mut t, 80, height, &opts);
        assert!(
            t.cells_laid_out <= height as usize,
            "scroll {scroll}: laid out {} blocks for a {height}-row viewport",
            t.cells_laid_out
        );
        assert!(t.cells_laid_out > 0);
    }

    // A changed cell is re-measured alone; nothing off screen is drawn.
    if let Some(cell) = t.cells.get_mut(n - 1) {
        cell.version = cell.version.wrapping_add(1);
    }
    t.scroll_from_bottom = 0;
    drawn(&mut t, 80, height, &opts);
    assert!(t.cells_laid_out <= height as usize);
    assert_eq!(t.extents.len(), blocks);
}

/// A line cache several times smaller than the transcript (as the 20,000-line
/// cache is under a long session): a frame at the tail renders nothing but
/// the viewport after the first, where a full sweep over an LRU smaller than
/// the sweep would miss on every block, every frame.
#[test]
fn a_long_transcript_does_not_thrash_the_line_cache() {
    let opts = RenderOptions::new(80, false);
    let mut t = build(11, 4_000, false, false);
    t.cache = LineCache::new(2_000);
    drawn(&mut t, 80, 30, &opts);
    let lines = t.rendered_lines.unwrap_or(0);
    assert!(
        lines > 2 * 2_000,
        "the transcript outgrows the cache: {lines}"
    );
    for _ in 0..5 {
        drawn(&mut t, 80, 30, &opts);
        assert!(
            t.cells_laid_out <= 30,
            "only the viewport's blocks: {}",
            t.cells_laid_out
        );
    }
}
