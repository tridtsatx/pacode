use super::*;

#[test]
fn adding_a_category_lands_in_that_category_and_the_total() {
    let mut totals = CategoryTotals::default();
    for (category, tokens) in [
        (TokenCategory::Instructions, 10),
        (TokenCategory::ToolDefs, 20),
        (TokenCategory::History, 30),
        (TokenCategory::ToolResults, 40),
        (TokenCategory::Reply, 50),
    ] {
        totals.add(category, tokens);
    }

    // Every arm of `add` has to reach its own field: a copy-paste that put
    // tool results into `history` would still sum to 150 and look fine.
    assert_eq!(totals.instructions, 10);
    assert_eq!(totals.tool_defs, 20);
    assert_eq!(totals.history, 30);
    assert_eq!(totals.tool_results, 40);
    assert_eq!(totals.reply, 50);
    assert_eq!(totals.total(), 150);
    assert_eq!(totals.rows().map(|(c, _)| c), TOKEN_CATEGORIES);
    for category in TOKEN_CATEGORIES {
        assert_eq!(totals.get(category), totals.rows()[category as usize].1);
    }
}

#[test]
fn a_lost_checkpoint_is_never_scored_as_a_miss() {
    // 10 checkpoints, 7 passed, 1 ignored, 1 wrong, 1 lost to compaction.
    // The lost one was never askable, so it must not sit in the denominator:
    // 7/9, not 7/10.
    let report = ContextQualityReport::scored(10, 7, 1, 1, 1);
    assert_eq!(report.scored_checkpoints(), 9);
    assert!(
        (report.score - 7.0 / 9.0).abs() < 1e-12,
        "score was {}",
        report.score
    );
}

#[test]
fn recording_checkpoints_keeps_the_score_in_step_with_the_counters() {
    let mut report = ContextQualityReport::default();
    assert!(
        !report.is_unscored(),
        "an empty session has no checkpoints at all"
    );

    for _ in 0..3 {
        report.record(CheckpointOutcome::Passed);
    }
    report.record(CheckpointOutcome::Wrong);
    report.record(CheckpointOutcome::Ignored);
    report.record(CheckpointOutcome::LostToCompaction);

    assert_eq!(report.checkpoints, 6);
    assert_eq!(report.passed, 3);
    assert_eq!(report.wrong, 1);
    assert_eq!(report.ignored, 1);
    assert_eq!(report.lost_to_compaction, 1);
    assert_eq!(report.scored_checkpoints(), 5);
    assert!(
        (report.score - 0.6).abs() < 1e-12,
        "score was {}",
        report.score
    );
    assert!(!report.is_unscored());
}

#[test]
fn a_session_where_every_checkpoint_was_compacted_away_reports_no_score() {
    let mut report = ContextQualityReport::default();
    report.record(CheckpointOutcome::LostToCompaction);
    report.record(CheckpointOutcome::LostToCompaction);

    assert_eq!(report.scored_checkpoints(), 0);
    assert!(report.is_unscored());
    // The page turns this into "no scored checkpoints" rather than a 0.00.
    assert_eq!(report.score, 0.0);
}

#[test]
fn requests_seen_counts_the_records_dropped_at_the_cap() {
    let report = AnalyzeReport {
        requests: vec![RequestRecord::default(); 3],
        dropped_requests: 41,
        ..AnalyzeReport::default()
    };
    assert_eq!(report.requests_detailed(), 3);
    assert_eq!(report.requests_seen(), 44);
}
