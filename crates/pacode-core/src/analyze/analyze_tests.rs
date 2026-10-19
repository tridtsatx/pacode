use std::time::Duration;

use pacode_types::metrics::LatencyHist;
use pacode_types::{
    AnalyzeReport, CategoryTotals, ContextQualityReport, REQUEST_RECORD_CAP, TOOL_NAME_CAP,
    TOOL_OVERFLOW_NAME, TokenCategory, ToolCost,
};

use super::{RequestAttribution, RequestLatency, SessionAccounting};

/// A request charged to a recognisable category split, with `marker` in the
/// provider-reported input so a test can tell one record from another.
fn request(instructions: u64, history: u64, tool_results: u64, marker: u64) -> RequestAttribution {
    RequestAttribution {
        categories: CategoryTotals {
            instructions,
            tool_defs: 1_000,
            history,
            tool_results,
            reply: 10,
        },
        reported_input: marker,
        reported_output: 10,
        cache_read: 0,
    }
}

/// The sum of two [`request`]s. Every field is passed in rather than baked in,
/// because a helper that hardcodes half the expectation cannot tell a summing
/// bug from a doubling bug — and the whole point of this test is the sum.
fn totals(
    instructions: u64,
    tool_defs: u64,
    history: u64,
    tool_results: u64,
    reply: u64,
) -> CategoryTotals {
    CategoryTotals {
        instructions,
        tool_defs,
        history,
        tool_results,
        reply,
    }
}

#[test]
fn the_session_totals_are_the_sum_of_the_requests_behind_them() {
    let ledger = SessionAccounting::new();
    ledger.record_request(request(100, 200, 30, 1), None);
    ledger.record_request(request(100, 400, 70, 2), None);

    let report = ledger.report();
    assert_eq!(report.categories, totals(200, 2_000, 600, 100, 20));
    assert_eq!(report.categories.total(), 200 + 2_000 + 600 + 100 + 20);
    assert_eq!(report.reported_input, 3, "1 + 2 markers");
    // The per-request rows are the same split, not a second, looser one: a
    // page that summed these two would have to agree with the total above.
    let from_rows = report
        .requests
        .iter()
        .fold(CategoryTotals::default(), |mut sum, record| {
            for (category, tokens) in record.categories.rows() {
                sum.add(category, tokens);
            }
            sum
        });
    assert_eq!(from_rows, report.categories);
    for (category, tokens) in report.categories.rows() {
        assert_eq!(report.categories.get(category), tokens);
        assert!(tokens > 0, "{category:?} went missing");
    }
}

#[test]
fn the_request_cap_drops_the_oldest_records_and_counts_them() {
    let ledger = SessionAccounting::new();
    let total = REQUEST_RECORD_CAP + 5;
    for index in 1..=total {
        ledger.record_request(request(1, index as u64, 0, index as u64), None);
    }

    let report = ledger.report();
    assert_eq!(report.requests_detailed(), REQUEST_RECORD_CAP);
    assert_eq!(report.dropped_requests, 5);
    assert_eq!(report.requests_seen(), total as u64);
    // Oldest first, and the five that fell off are the *first* five — a
    // pop_back here would silently show the user a session that never happened.
    assert_eq!(report.requests[0].reported_input, 6);
    assert_eq!(
        report.requests[REQUEST_RECORD_CAP - 1].reported_input,
        total as u64
    );
    // The dropped records are gone from the detail but not from the bill.
    assert_eq!(
        report.categories.history,
        (1..=total as u64).sum::<u64>(),
        "a dropped record must not shrink the session total"
    );
    assert_eq!(report.categories.instructions, total as u64);
}

#[test]
fn the_tool_ranking_is_ordered_by_volume_and_an_empty_session_ranks_nothing() {
    let empty = SessionAccounting::new().report();
    assert!(empty.tools.is_empty());
    assert_eq!(empty.tool_result_tokens, 0);
    assert_eq!(empty.requests_seen(), 0);
    assert_eq!(empty.quality, ContextQualityReport::default());
    assert_eq!(empty.ttft.count, 0);

    let ledger = SessionAccounting::new();
    ledger.record_tool_call("read", 1_000);
    ledger.record_tool_call("grep", 900_000);
    ledger.record_tool_call("grep", 100_000);
    ledger.record_tool_call("bash", 500_000);
    // Same volume as `read`: the name breaks the tie so the page does not
    // shuffle two identical rows between frames.
    ledger.record_tool_call("apply_patch", 1_000);

    let report = ledger.report();
    let ranked: Vec<(&str, u64)> = report
        .tools
        .iter()
        .map(|tool| (tool.tool.as_str(), tool.result_tokens))
        .collect();
    assert_eq!(
        ranked,
        vec![
            ("grep", 1_000_000),
            ("bash", 500_000),
            ("apply_patch", 1_000),
            ("read", 1_000),
        ]
    );
    assert_eq!(report.tools[0].calls, 2, "both grep calls on one row");
    assert_eq!(
        report
            .tools
            .iter()
            .map(|tool| tool.result_tokens)
            .sum::<u64>(),
        report.tool_result_tokens
    );
}

#[test]
fn a_tool_name_past_the_cap_is_folded_not_dropped() {
    let ledger = SessionAccounting::new();
    for index in 0..TOOL_NAME_CAP {
        ledger.record_tool_call(&format!("tool_{index}"), 10);
    }
    ledger.record_tool_call("one_too_many", 7_000);

    let report = ledger.report();
    assert_eq!(
        report.tools.len(),
        TOOL_NAME_CAP + 1,
        "the fold has its own row"
    );
    assert_eq!(report.folded_tools, 1);
    let overflow = report
        .tools
        .iter()
        .find(|tool| tool.tool == TOOL_OVERFLOW_NAME)
        .expect("folded calls must still be on the page");
    assert_eq!(overflow.result_tokens, 7_000);
    assert_eq!(overflow.calls, 1);
    assert_eq!(
        report
            .tools
            .iter()
            .map(|tool| tool.result_tokens)
            .sum::<u64>(),
        report.tool_result_tokens,
        "folding a name must never lose tokens"
    );
    // A name already on a row keeps its own row even once the map is full.
    ledger.record_tool_call("tool_0", 5);
    let report = ledger.report();
    assert_eq!(report.folded_tools, 1);
    assert!(report.tools.iter().any(|tool| tool.tool == "tool_0"));
}

#[test]
fn latency_percentiles_are_the_ones_pacode_stats_prints() {
    let samples = [
        Duration::from_micros(1_000),
        Duration::from_micros(2_000),
        Duration::from_micros(50_000),
        Duration::from_micros(900_000),
    ];

    let ledger = SessionAccounting::new();
    for sample in samples {
        ledger.record_request(
            request(0, 0, 0, 0),
            Some(RequestLatency {
                ttft: Some(sample),
                total: sample * 2,
            }),
        );
    }

    // The same samples through the same histogram `pacode stats` uses, so a
    // quantile here and a quantile there cannot disagree.
    let ttft_hist = LatencyHist::new();
    let total_hist = LatencyHist::new();
    for sample in samples {
        ttft_hist.record_duration(sample);
        total_hist.record_duration(sample * 2);
    }
    let expected_ttft = ttft_hist.snapshot();
    let expected_total = total_hist.snapshot();

    let report = ledger.report();
    assert_eq!(report.ttft, expected_ttft);
    assert_eq!(report.total_latency, expected_total);
    for quantile in [0.5, 0.9, 0.99] {
        assert_eq!(
            report.ttft.quantile_micros(quantile),
            expected_ttft.quantile_micros(quantile)
        );
        assert_eq!(
            report.total_latency.quantile_micros(quantile),
            expected_total.quantile_micros(quantile)
        );
    }
    // Pinned to the log2 bucket edges, not to "whatever came out": bucket 10
    // holds [1024, 2048) µs, so a median of 2000 µs reports 2048.
    assert_eq!(report.ttft.quantile_micros(0.50), 2_048);
    assert_eq!(report.ttft.quantile_micros(0.99), 1 << 20);
    assert_eq!(report.ttft.count, 4);
    assert_eq!(report.total_latency.max_micros, 1_800_000);
}

#[test]
fn a_request_that_never_produced_a_token_is_absent_from_ttft_not_a_fast_one() {
    let ledger = SessionAccounting::new();
    ledger.record_request(
        request(0, 0, 0, 0),
        Some(RequestLatency {
            ttft: None,
            total: Duration::from_millis(40),
        }),
    );

    let report = ledger.report();
    assert_eq!(report.ttft.count, 0, "no first token means no TTFT sample");
    assert_eq!(report.total_latency.count, 1);
    assert_eq!(report.requests[0].ttft_micros, 0);
    assert_eq!(report.requests[0].total_micros, 40_000);
}

#[test]
fn the_context_quality_slot_takes_a_report_and_an_incremental_edit() {
    let ledger = SessionAccounting::new();
    ledger.record_context_quality(ContextQualityReport::scored(4, 3, 0, 1, 0));
    ledger.update_context_quality(|report| {
        report.record(pacode_types::CheckpointOutcome::Passed);
    });

    let report = ledger.report();
    assert_eq!(report.quality.checkpoints, 5);
    assert_eq!(report.quality.passed, 4);
    // Points, not a ratio: three correct minus one wrong, plus the one just
    // recorded. A ratio would call four-out-of-five "0.8" and hide the fact that
    // a wrong word costs a point.
    assert!(
        (report.quality.score - 3.0).abs() < 1e-12,
        "{}",
        report.quality.score
    );
}

#[test]
fn the_default_report_is_what_the_page_draws_before_the_first_request() {
    let report = AnalyzeReport::default();
    assert_eq!(report.requests_seen(), 0);
    assert!(report.tools.is_empty());
    assert_eq!(report.categories.total(), 0);
    assert_eq!(report.categories.get(TokenCategory::Reply), 0);
    assert_eq!(ToolCost::default().result_tokens, 0);
}
