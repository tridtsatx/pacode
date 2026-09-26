use pacode_render::RenderOptions;
use pacode_types::metrics::LatencyHist;
use pacode_types::{
    AnalyzeReport, CategoryTotals, ContextQualityReport, RequestRecord, TOOL_OVERFLOW_NAME,
    ToolCost,
};

use super::*;

/// A session that has done real work: every section has something to say, and
/// the caps have both been hit.
pub(super) fn busy_report() -> AnalyzeReport {
    let ttft = LatencyHist::new();
    let total = LatencyHist::new();
    for micros in [12_000u64, 340_000, 1_900_000, 88_000, 260_000] {
        ttft.record(micros);
        total.record(micros * 4);
    }
    let mut tools: Vec<ToolCost> = (0..25)
        .map(|index| ToolCost {
            tool: format!("mcp__a_very_long_server_name__tool_number_{index}"),
            calls: index as u64 + 1,
            result_tokens: 4_000 * (25 - index as u64),
        })
        .collect();
    tools.push(ToolCost {
        tool: TOOL_OVERFLOW_NAME.to_string(),
        calls: 3,
        result_tokens: 900,
    });

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
        tools,
        folded_tools: 3,
        tool_result_tokens: 6_400_000,
        ttft: ttft.snapshot(),
        total_latency: total.snapshot(),
        quality: ContextQualityReport::scored(12, 9, 1, 1, 1),
    }
}

fn opts() -> RenderOptions {
    RenderOptions::new(80, false)
}

fn plain(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<String>>()
        .join("\n")
}

#[test]
fn no_row_is_wider_than_the_space_it_was_given() {
    let report = busy_report();
    let opts = opts();
    // 80x24 is the floor the spec calls out; the widths either side of it are
    // where a right-aligned number or a long tool name would push a row over.
    for width in [12usize, 20, 40, 60, 78, 120] {
        let lines = build(&report, width, &opts);
        for line in &lines {
            let rendered: usize = line
                .spans
                .iter()
                .map(|span| pacode_render::display_width(&span.content))
                .sum();
            assert!(
                rendered <= width,
                "row of {rendered} cells into {width}: {:?}",
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            );
        }
    }
}

#[test]
fn the_estimate_caveat_sits_with_the_numbers_it_qualifies() {
    let lines = build(&busy_report(), 78, &opts());
    let text = plain(&lines);
    assert!(
        text.contains("est. chars/4"),
        "the per-category rows are estimates and must say so:\n{text}"
    );
    assert!(text.contains("provider reported"));
    // The four estimated rows and the one measured row are all present.
    for label in [
        "instructions",
        "tool defs",
        "history",
        "tool results",
        "reply",
    ] {
        assert!(text.contains(label), "missing row {label}:\n{text}");
    }
}

#[test]
fn the_cap_drop_is_visible_on_the_page() {
    let text = plain(&build(&busy_report(), 78, &opts()));
    assert!(
        text.contains("1487 older dropped at the 512 cap"),
        "a per-request tail must announce itself:\n{text}"
    );
    assert!(
        text.contains("still cover every request"),
        "the totals are whole-session and the page should say so:\n{text}"
    );
    assert!(text.contains("tool calls folded into one row"));
}

#[test]
fn the_heaviest_tool_leads_and_long_names_are_clipped_not_wrapped() {
    let text = plain(&build(&busy_report(), 78, &opts()));
    let first = text
        .lines()
        .find(|line| line.contains("mcp__a_very_long_server_name__tool_number_0"))
        .expect("the heaviest tool must be listed");
    assert!(first.contains("100.0k"), "got: {first}");
    // 25 tools plus the folded row is more than the cap: the page says so
    // instead of pretending the list ends there.
    assert!(text.contains("more tool rows further down"), "{text}");
}

#[test]
fn an_empty_session_reads_as_empty_rather_than_as_zeroes() {
    let report = AnalyzeReport::default();
    let text = plain(&build(&report, 78, &opts()));
    assert!(text.contains("no requests recorded yet"), "{text}");
    assert!(text.contains("no tool results yet"), "{text}");
    assert!(text.contains("no completed requests yet"), "{text}");
    assert!(text.contains("no checkpoints run yet"), "{text}");
}

#[test]
fn a_session_where_every_checkpoint_was_compacted_away_shows_no_score() {
    let report = AnalyzeReport {
        quality: ContextQualityReport::scored(4, 0, 0, 0, 4),
        ..AnalyzeReport::default()
    };
    let text = plain(&build(&report, 78, &opts()));

    assert!(text.contains("4 lost to compaction"), "{text}");
    assert!(
        text.contains("score: not scored"),
        "a 0% here would blame the model for compaction:\n{text}"
    );
    assert!(!text.contains("score 0%"), "{text}");
}

#[test]
fn latency_rows_carry_the_quantiles_pacode_stats_would_print() {
    let report = busy_report();
    let opts = opts();
    let text = plain(&build(&report, 78, &opts));
    assert!(text.contains("n=5"), "{text}");
    assert!(text.contains("p50"), "{text}");
    assert!(text.contains("p99"), "{text}");
    // The same histogram, so the numbers cannot drift from `pacode stats`.
    let p99 = report.ttft.quantile_micros(0.99);
    assert!(text.contains(&pacode_types::time::format_duration_ms(p99 / 1_000)));
}
