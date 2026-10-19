use super::*;
use pacode_types::transcript::ToolStatus;
use pacode_types::{ToastLevel, TranscriptKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

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

fn random_kind(rng: &mut Rng) -> CellKind {
    let pick = rng.below(7);
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
        5 => CellKind::Item(TranscriptKind::ToolCall {
            call_id: "c".into(),
            name: "bash".into(),
            title: "bash ls".into(),
            intent: None,
            status: ToolStatus::Ok,
            preview: words(rng, n),
            diff: None,
            duration_ms: Some(5),
            task: None,
        }),
        _ => CellKind::Item(TranscriptKind::ToolCall {
            call_id: "r".into(),
            name: "bash".into(),
            title: "bash sleep".into(),
            intent: None,
            status: ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: Some(10),
            task: None,
        }),
    }
}

fn build(seed: u64, n: usize, header: bool) -> Transcript {
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
            kind: random_kind(&mut rng),
            version: rng.below(3) as u32,
            ts_ms: i as u64,
            stats,
        });
    }
    // One expanded cell, so the expansion bit is exercised.
    if n > 3 {
        t.expanded.insert(3);
    }
    t
}

fn drawn(t: &mut Transcript, w: u16, h: u16, opts: &RenderOptions) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    term.draw(|f| draw(f, f.area(), t, opts, 7, 10_000))
        .expect("draw");
    term.backend().buffer().clone()
}

/// The old algorithm: render everything, then pick the viewport out of it.
fn reference(t: &mut Transcript, w: u16, h: u16, opts: &RenderOptions) -> Buffer {
    let all = all_lines(t, w, opts, 7, 10_000);
    let viewport = h as usize;
    let max_scroll = all.len().saturating_sub(viewport);
    let scroll = t.scroll_from_bottom.min(max_scroll);
    let start = max_scroll.saturating_sub(scroll);
    let mut visible: Vec<Line<'static>> = all.into_iter().skip(start).take(viewport).collect();
    if scroll > 0
        && let Some(last) = visible.last_mut()
    {
        *last = Line::from(Span::styled(
            format!("↓ {scroll} new lines"),
            opts.theme.dim,
        ));
    }
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    term.draw(|f| {
        f.render_widget(Paragraph::new(visible), Rect::new(0, 0, w, h));
    })
    .expect("draw");
    term.backend().buffer().clone()
}

#[test]
fn windowed_draw_matches_full_render_at_several_widths() {
    for seed in [1u64, 7, 42, 1234, 99_991] {
        for header in [false, true] {
            for w in [12u16, 40, 80, 133] {
                let opts = RenderOptions::new(w, false);
                for h in [1u16, 7, 24] {
                    for scroll in [0usize, 1, 5, 40, 100_000] {
                        let mut a = build(seed, 60, header);
                        let mut b = build(seed, 60, header);
                        a.scroll_from_bottom = scroll;
                        b.scroll_from_bottom = scroll;
                        // Two frames: the second answers from the measured counts.
                        for frame in 0..2 {
                            let got = drawn(&mut a, w, h, &opts);
                            let want = reference(&mut b, w, h, &opts);
                            assert_eq!(
                                got, want,
                                "seed {seed} header {header} w {w} h {h} scroll {scroll} frame {frame}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn click_map_is_contiguous_and_covers_every_rendered_line() {
    let opts = RenderOptions::new(60, false);
    for header in [false, true] {
        let mut t = build(5, 40, header);
        drawn(&mut t, 60, 10, &opts);
        let total = all_lines(&mut t, 60, &opts, 7, 10_000).len();
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
    let mut t = build(9, 30, false);
    for w in [30u16, 90, 30] {
        let opts = RenderOptions::new(w, false);
        let got = drawn(&mut t, w, 12, &opts);
        let mut fresh = build(9, 30, false);
        assert_eq!(got, reference(&mut fresh, w, 12, &opts), "width {w}");
    }
    t.cache.clear();
    let opts = RenderOptions::new(30, false);
    let got = drawn(&mut t, 30, 12, &opts);
    let mut fresh = build(9, 30, false);
    assert_eq!(got, reference(&mut fresh, 30, 12, &opts));
}

#[test]
fn scrolling_a_huge_transcript_lays_out_only_the_viewport() {
    let n = 10_000;
    let height = 40u16;
    let opts = RenderOptions::new(80, false);
    let mut t = build(77, n, false);
    drawn(&mut t, 80, height, &opts);
    assert_eq!(
        t.cells_laid_out, n,
        "the first frame measures every cell once"
    );

    for scroll in [0usize, 1, 37, 500, 5_000, 20_000, 100_000, 3, 0] {
        t.scroll_from_bottom = scroll;
        drawn(&mut t, 80, height, &opts);
        assert!(
            t.cells_laid_out <= height as usize,
            "scroll {scroll}: laid out {} cells for a {height}-row viewport",
            t.cells_laid_out
        );
        assert!(t.cells_laid_out > 0);
    }

    // A changed cell is re-measured alone.
    if let Some(cell) = t.cells.get_mut(n - 1) {
        cell.version = cell.version.wrapping_add(1);
    }
    t.scroll_from_bottom = 0;
    drawn(&mut t, 80, height, &opts);
    assert!(t.cells_laid_out <= height as usize);
    assert_eq!(t.extents.len(), n);
}
