use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::text::Line;

use pacode_types::{AnalyzeReport, CategoryTotals, ContextQualityReport, RequestRecord, ToolCost};

use super::*;

/// A session long enough that the page overflows an 80x24 screen several times
/// over, with the caps already hit and a checkpoint in the book.
fn busy_report() -> AnalyzeReport {
    let ttft = pacode_types::metrics::LatencyHist::new();
    let total = pacode_types::metrics::LatencyHist::new();
    for micros in [12_000u64, 340_000, 1_900_000, 88_000, 260_000] {
        ttft.record(micros);
        total.record(micros * 4);
    }
    AnalyzeReport {
        categories: CategoryTotals {
            instructions: 1_240_000,
            tool_defs: 890_000,
            history: 4_100_000,
            tool_results: 6_400_000,
            reply: 96_400,
        },
        reported_input: 12_730_000,
        reported_output: 71_200,
        cache_read: 9_100_000,
        requests: vec![RequestRecord {
            categories: CategoryTotals {
                instructions: 1_000,
                tool_defs: 800,
                history: 2_000,
                tool_results: 3_000,
                reply: 40,
            },
            reported_input: 6_840,
            cache_read: 4_000,
            ttft_micros: 340_000,
            total_micros: 1_360_000,
        }],
        dropped_requests: 1_487,
        tools: vec![
            ToolCost {
                tool: "mcp__an_extremely_long_server_name__search".to_string(),
                calls: 12,
                result_tokens: 900_000,
            },
            ToolCost {
                tool: "grep".to_string(),
                calls: 41,
                result_tokens: 640_000,
            },
        ],
        folded_tools: 0,
        tool_result_tokens: 1_540_000,
        ttft: ttft.snapshot(),
        total_latency: total.snapshot(),
        quality: ContextQualityReport::scored(12, 9, 1, 1, 1),
    }
}

fn opts(cols: u16) -> RenderOptions {
    RenderOptions::new(cols, false)
}

/// The page as the terminal sees it: one string per row, trailing blanks kept
/// off.
fn render(report: &AnalyzeReport, view: &AnalyzeView, area: Rect) -> Vec<String> {
    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = Terminal::new(backend).expect("test backend");
    let opts = opts(area.width);
    terminal
        .draw(|frame| draw(frame, area, report, view, &opts))
        .expect("the page must draw without panicking");
    let buffer = terminal.backend().buffer();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(""))
                .collect::<String>()
        })
        .map(|row| row.trim_end().to_string())
        .collect()
}

fn built_text(report: &AnalyzeReport, width: usize, opts: &RenderOptions) -> Vec<String> {
    rows::build(report, width, opts)
        .iter()
        .map(line_text)
        .collect()
}

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The content rows of a drawn page, frame stripped.
///
/// `draw` puts a border and a title around the rows, so a screen row and a
/// built row are the same line only between the two frame columns. Compared
/// whole, the border would be checked against content.
fn content_rows(report: &AnalyzeReport, view: &AnalyzeView, area: Rect) -> Vec<String> {
    render(report, view, area)
        .into_iter()
        .skip(1)
        .take(area.height as usize - 2)
        .map(|row| {
            row.chars()
                .skip(1)
                .take(area.width as usize - 2)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn the_page_fits_80x24_with_one_row_per_row() {
    let report = busy_report();
    let area = Rect::new(0, 0, 80, 24);
    let view = AnalyzeView::new();
    let screen = render(&report, &view, area);

    assert_eq!(screen.len(), 24);
    assert!(
        screen[0].contains(TITLE),
        "border title missing: {:?}",
        screen[0]
    );
    assert!(
        screen[23].contains("esc close"),
        "hint missing: {:?}",
        screen[23]
    );

    // The decisive check: the 22 content rows on screen are exactly the 22
    // rows the layout built, in order. A row that wrapped would push its tail
    // onto the next line and break this equality — which is the whole reason
    // the page clips instead of wrapping.
    let opts = opts(80);
    let built = built_text(&report, 78, &opts);
    assert!(
        built.len() > 22,
        "the fixture must overflow the screen or this proves nothing"
    );
    let expected: Vec<String> = built[..22].to_vec();
    let actual = content_rows(&report, &view, area);
    assert_eq!(actual, expected);

    // And nothing spilled past the right edge.
    for row in &screen {
        assert!(
            pacode_render::display_width(row) <= 80,
            "row wider than the terminal: {row:?}"
        );
    }
    assert!(screen.iter().any(|row| row.contains("Token spend")));
    assert!(screen.iter().any(|row| row.contains("est. chars/4")));
}

#[test]
fn scrolling_reaches_the_footer_and_stops_there() {
    let report = busy_report();
    let area = Rect::new(0, 0, 80, 24);
    let opts = opts(80);
    let built = built_text(&report, 78, &opts);
    let last = built.len() - 1;

    let mut view = AnalyzeView::new();
    view.clamp(built.len(), visible_rows(area));
    view.scroll = u16::MAX;
    let screen = render(&report, &view, area);

    assert_eq!(
        content_rows(&report, &view, area)[21],
        built[last],
        "the last row must sit on the fold"
    );
    assert!(
        screen.iter().any(|row| row.contains("Coverage")),
        "the coverage section is the last thing on the page: {screen:?}"
    );
    assert!(
        screen
            .iter()
            .any(|row| row.contains("older dropped at the 512"))
    );
}

#[test]
fn a_stale_scroll_offset_lands_on_the_last_row_instead_of_blank_space() {
    // The user left the page scrolled deep; the session restarted and the
    // report is now short. The draw clamps on its own so the page cannot show
    // empty rows under the content.
    let report = AnalyzeReport {
        quality: ContextQualityReport::scored(3, 3, 0, 0, 0),
        ..AnalyzeReport::default()
    };
    let area = Rect::new(0, 0, 80, 24);
    let view = AnalyzeView { scroll: 4_000 };
    let screen = render(&report, &view, area);
    let built = built_text(&report, 78, &opts(80));

    // The property is not "the last row sits at index 21" — a page shorter than
    // the screen sits at the top, and pretending otherwise would only pass for
    // a full page. It is that the built rows are on screen in order, from the
    // first, and that nothing but blanks follows them.
    let rows = content_rows(&report, &view, area);
    assert_eq!(
        &rows[..built.len()],
        &built[..],
        "the page must not be scrolled"
    );
    assert!(
        rows[built.len()..].iter().all(|row| row.is_empty()),
        "a clamped page leaves blanks, not missing content: {rows:?}"
    );
    // Three correct and nothing else is three points, and the page says so
    // with a sign — a run that ignored half its checkpoints must not read as
    // the same "100%".
    assert!(
        screen.iter().any(|row| row.contains("score +3")),
        "{screen:?}"
    );
}

#[test]
fn an_area_too_small_to_hold_the_page_draws_nothing_and_does_not_panic() {
    let report = busy_report();
    for area in [
        Rect::new(0, 0, 0, 0),
        Rect::new(0, 0, 4, 2),
        Rect::new(0, 0, 11, 24),
        Rect::new(0, 0, 80, 3),
    ] {
        let screen = render(&report, &AnalyzeView::new(), area);
        assert!(
            screen.iter().all(|row| row.is_empty()),
            "{area:?} should stay blank, got {screen:?}"
        );
    }
}

#[test]
fn a_tall_terminal_shows_the_whole_page_without_a_scrollbar_of_blank_rows() {
    let report = busy_report();
    let opts = opts(80);
    let built = built_text(&report, 78, &opts);
    let height = u16::try_from(built.len() + 2).expect("fixture fits u16");
    // Checked through the frame-stripped rows below; the bordered screen itself
    // is not part of this assertion.

    assert_eq!(
        content_rows(&report, &AnalyzeView::new(), Rect::new(0, 0, 80, height)),
        built[..]
    );
    assert_eq!(visible_rows(Rect::new(0, 0, 80, height)), built.len());
}
