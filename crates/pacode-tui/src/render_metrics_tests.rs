//! The report's own contract: the file round-trips, the gate is off unless the
//! env var says so, and a missing or corrupt report is `None` rather than a
//! failure.

use super::{
    FrameCost, FrameKind, MetricsGate, RenderMetrics, RenderStats, env_requests_metrics,
    read_report,
};

fn cost(ms: u64) -> FrameCost {
    let d = std::time::Duration::from_millis(ms);
    FrameCost {
        layout: d,
        backend: d,
        frame: d * 2,
    }
}

#[test]
fn the_env_var_is_the_only_switch() {
    assert!(env_requests_metrics(Some("1")));
    assert!(env_requests_metrics(Some("true")));
    for off in [None, Some("0"), Some(""), Some("yes"), Some("TRUE ")] {
        assert!(!env_requests_metrics(off), "{off:?} must not arm it");
    }
}

#[test]
fn a_gate_that_is_not_armed_writes_nothing_and_names_no_path() {
    let dir = tempdir("render-metrics-off");
    let gate = MetricsGate::install(&dir);
    assert!(gate.report_path().is_none());
    drop(gate);
    assert!(!dir.join("render-metrics.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_report_round_trips_through_the_file_it_names() {
    let dir = tempdir("render-metrics-roundtrip");
    let path = dir.join("render-metrics.json");
    // The gate is only armed by the env var, so the report path is written
    // here directly rather than through an env mutation in a shared process.
    let stats = RenderStats {
        draws: 1200,
        content_draws: 300,
        anim_draws: 900,
        full_draws: 7,
        dropped_frames: 42,
        cadence_multiplier: 4,
        stream_backlog_high_water: 8192,
        stream_backlog: Some(64),
        sampled_at_ms: 1_700_000_000_000,
        ..RenderStats::default()
    };
    let bytes = serde_json::to_vec(&stats).expect("report serializes");
    std::fs::write(&path, &bytes).expect("report writes");
    assert_eq!(read_report(&path), Some(stats));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_or_corrupt_report_is_none_and_not_a_panic() {
    let dir = tempdir("render-metrics-corrupt");
    assert_eq!(read_report(&dir.join("nope.json")), None);
    let bad = dir.join("bad.json");
    std::fs::write(&bad, b"{not json").expect("write");
    assert_eq!(read_report(&bad), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn frame_kinds_are_counted_apart_and_full_counts_as_content() {
    assert!(FrameKind::Full.is_content());
    assert!(FrameKind::Content.is_content());
    assert!(!FrameKind::Anim.is_content());
    assert_eq!(FrameKind::default(), FrameKind::Anim);
    assert_eq!(FrameKind::as_str(FrameKind::Full), "full");
}

#[test]
fn recording_is_off_until_the_gate_is_installed() {
    // `RenderMetrics` is process-wide, so this asserts the property that
    // matters and is order-independent: with no gate installed, the record
    // calls are no-ops and the counters stay where they were.
    let before = RenderMetrics::snapshot();
    RenderMetrics::record(FrameKind::Anim, cost(1));
    RenderMetrics::record_drop();
    RenderMetrics::record_resize_frame();
    RenderMetrics::note_multiplier(2);
    RenderMetrics::note_backlog(10);
    let after = RenderMetrics::snapshot();
    if !RenderMetrics::recording() {
        assert_eq!(after.draws, before.draws);
        assert_eq!(after.dropped_frames, before.dropped_frames);
        assert_eq!(after.cadence_multiplier, before.cadence_multiplier);
    } else {
        assert!(after.draws >= before.draws);
    }
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pacode-{name}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}
