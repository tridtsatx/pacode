//! Tests for `crate::schedule`: schedule parsing, monitor normalization, summaries.

use crate::ids::{CronJobId, MonitorId};
use crate::schedule::{CronJob, CronSchedule, MonitorCondition, MonitorInfo, MonitorStatus};

#[test]
fn cron_schedule_parse_interval_forms() {
    assert_eq!(
        CronSchedule::parse("every 90s"),
        Some(CronSchedule::Every { secs: 90 })
    );
    assert_eq!(
        CronSchedule::parse("every 5m"),
        Some(CronSchedule::Every { secs: 300 })
    );
    assert_eq!(
        CronSchedule::parse("every 2h"),
        Some(CronSchedule::Every { secs: 7_200 })
    );
    assert_eq!(
        CronSchedule::parse("every 1d"),
        Some(CronSchedule::Every { secs: 86_400 })
    );
    assert_eq!(
        CronSchedule::parse("every 30"),
        Some(CronSchedule::Every { secs: 30 })
    );
    assert_eq!(
        CronSchedule::parse("@every 5m"),
        Some(CronSchedule::Every { secs: 300 })
    );
    // below the floor: clamped to 1 second, never 0
    assert_eq!(
        CronSchedule::parse("every 0"),
        Some(CronSchedule::Every { secs: 1 })
    );
}

#[test]
fn cron_schedule_parse_expression_forms() {
    assert_eq!(
        CronSchedule::parse("*/15 * * * *"),
        Some(CronSchedule::cron("*/15 * * * *"))
    );
    assert_eq!(
        CronSchedule::parse("0 9 * * MON-FRI"),
        Some(CronSchedule::cron("0 9 * * MON-FRI"))
    );
    assert_eq!(
        CronSchedule::parse("0 */10 * * * *"),
        Some(CronSchedule::cron("0 */10 * * * *"))
    );
    assert_eq!(CronSchedule::parse(""), None);
    assert_eq!(CronSchedule::parse("   "), None);
    assert_eq!(CronSchedule::parse("soon"), None);
    assert_eq!(CronSchedule::parse("every nope"), None);
    // four fields are not a cron expression and not an interval
    assert_eq!(CronSchedule::parse("* * * *"), None);
}

#[test]
fn cron_schedule_display_round_trips() {
    let cases = [
        (CronSchedule::Every { secs: 90 }, "every 90s"),
        (CronSchedule::Every { secs: 300 }, "every 5m"),
        (CronSchedule::Every { secs: 7_200 }, "every 2h"),
        (CronSchedule::Every { secs: 86_400 }, "every 1d"),
        (CronSchedule::Every { secs: 45 }, "every 45s"),
        (CronSchedule::cron("0 9 * * *"), "0 9 * * *"),
    ];
    for (schedule, text) in cases {
        assert_eq!(schedule.display(), text);
        assert_eq!(CronSchedule::parse(text), Some(schedule));
    }
}

#[test]
fn cron_schedule_serde_is_tagged() {
    let every = serde_json::to_string(&CronSchedule::Every { secs: 60 }).unwrap();
    assert_eq!(every, r#"{"kind":"every","secs":60}"#);
    let cron = serde_json::to_string(&CronSchedule::cron("0 9 * * *")).unwrap();
    assert_eq!(cron, r#"{"kind":"cron","expr":"0 9 * * *"}"#);
    let back: CronSchedule = serde_json::from_str(&cron).unwrap();
    assert_eq!(back, CronSchedule::cron("0 9 * * *"));
}

#[test]
fn cron_job_serde_round_trip_with_optional_fields_absent() {
    let job = CronJob {
        id: CronJobId::new("cron_1"),
        name: "nightly tests".into(),
        schedule: CronSchedule::cron("0 3 * * *"),
        prompt: "run the test suite and report".into(),
        enabled: true,
        created_at_ms: 1_000,
        last_run_ms: None,
        next_run_ms: None,
        last_status: None,
    };
    let json = serde_json::to_string(&job).unwrap();
    assert!(!json.contains("last_run_ms"));
    let back: CronJob = serde_json::from_str(&json).unwrap();
    assert_eq!(back, job);

    // fields written by an older daemon stay readable
    let legacy = r#"{
        "id": "cron_2",
        "name": "n",
        "schedule": {"kind": "every", "secs": 60},
        "prompt": "p",
        "enabled": false,
        "created_at_ms": 5
    }"#;
    let parsed: CronJob = serde_json::from_str(legacy).unwrap();
    assert!(!parsed.enabled);
    assert_eq!(parsed.last_status, None);
    assert_eq!(parsed.next_run_ms, None);
}

#[test]
fn cron_job_due_in_ignores_disabled_jobs() {
    let mut job = CronJob {
        id: CronJobId::new("cron_3"),
        name: "n".into(),
        schedule: CronSchedule::every(60),
        prompt: "p".into(),
        enabled: true,
        created_at_ms: 0,
        last_run_ms: None,
        next_run_ms: Some(10_000),
        last_status: None,
    };
    assert_eq!(job.due_in_ms(4_000), Some(6_000));
    // a run that slipped past `now` is due immediately, never negative
    assert_eq!(job.due_in_ms(20_000), Some(0));
    job.enabled = false;
    assert_eq!(job.due_in_ms(4_000), None);
}

#[test]
fn cron_job_summary_line_carries_state_and_name() {
    let job = CronJob {
        id: CronJobId::new("cron_4"),
        name: "standup".into(),
        schedule: CronSchedule::every(60),
        prompt: "p".into(),
        enabled: false,
        created_at_ms: 0,
        last_run_ms: Some(1),
        next_run_ms: None,
        last_status: Some("queued".into()),
    };
    let line = job.summary_line();
    assert!(line.contains("cron_4"), "{line}");
    assert!(line.contains("[off]"), "{line}");
    assert!(line.contains("standup"), "{line}");
    assert!(line.contains("queued"), "{line}");
}

#[test]
fn monitor_condition_from_parts_requires_its_params() {
    assert_eq!(
        MonitorCondition::from_parts("command_succeeds", Some("true"), None, None),
        Some(MonitorCondition::CommandSucceeds {
            command: "true".into()
        })
    );
    assert_eq!(
        MonitorCondition::from_parts("process_gone", None, Some("cargo"), None),
        Some(MonitorCondition::ProcessGone {
            pattern: "cargo".into()
        })
    );
    assert_eq!(
        MonitorCondition::from_parts("file_exists", None, None, Some("/tmp/x")),
        Some(MonitorCondition::FileExists {
            path: "/tmp/x".into()
        })
    );
    assert_eq!(
        MonitorCondition::from_parts("file_matches", None, Some("DONE"), Some("/tmp/x")),
        Some(MonitorCondition::FileMatches {
            path: "/tmp/x".into(),
            pattern: "DONE".into()
        })
    );
    // a missing parameter is an error, not a silently empty condition
    assert_eq!(
        MonitorCondition::from_parts("command_succeeds", None, None, None),
        None
    );
    assert_eq!(
        MonitorCondition::from_parts("file_matches", None, Some("x"), None),
        None
    );
    assert_eq!(
        MonitorCondition::from_parts("telepathy", None, None, None),
        None
    );
}

#[test]
fn monitor_condition_kind_and_describe_cover_every_variant() {
    let cases = [
        (
            MonitorCondition::CommandSucceeds {
                command: "cargo test".into(),
            },
            "command_succeeds",
            "`cargo test` exits 0",
            true,
        ),
        (
            MonitorCondition::ProcessGone {
                pattern: "cargo".into(),
            },
            "process_gone",
            "no process matches cargo",
            true,
        ),
        (
            MonitorCondition::FileExists {
                path: "/tmp/done".into(),
            },
            "file_exists",
            "/tmp/done exists",
            false,
        ),
        (
            MonitorCondition::FileMatches {
                path: "/tmp/log".into(),
                pattern: "OK".into(),
            },
            "file_matches",
            "/tmp/log matches OK",
            false,
        ),
    ];
    for (condition, kind, described, needs_exec) in cases {
        assert_eq!(condition.kind(), kind);
        assert_eq!(condition.describe(), described);
        assert_eq!(condition.needs_exec(), needs_exec);
        let json = serde_json::to_string(&condition).unwrap();
        assert!(json.contains(&format!("\"kind\":\"{kind}\"")), "{json}");
        let back: MonitorCondition = serde_json::from_str(&json).unwrap();
        assert_eq!(back, condition);
    }
}

#[test]
fn monitor_status_strings_and_liveness() {
    let cases = [
        (MonitorStatus::Watching, "watching", true),
        (MonitorStatus::Fired, "fired", false),
        (MonitorStatus::Stopped, "stopped", false),
        (MonitorStatus::Failed, "failed", false),
    ];
    for (status, text, live) in cases {
        assert_eq!(status.as_str(), text);
        assert_eq!(status.is_live(), live);
        assert_eq!(status.is_terminal(), !live);
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            format!("\"{text}\"")
        );
    }
}

fn monitor(status: MonitorStatus, started_at_ms: u64, last_check_ms: Option<u64>) -> MonitorInfo {
    MonitorInfo {
        id: MonitorId::new("mon_1"),
        label: "build finished".into(),
        condition: MonitorCondition::FileExists {
            path: "/tmp/done".into(),
        },
        poll_interval_secs: 5,
        started_at_ms,
        status,
        last_check_ms,
        fired_at_ms: None,
    }
}

#[test]
fn monitor_next_check_counts_from_start_then_from_last_check() {
    // A monitor that has never been checked is due at once: a condition that
    // already holds must not wait out a full interval first.
    let fresh = monitor(MonitorStatus::Watching, 1_000, None);
    assert_eq!(fresh.next_check_at_ms(), Some(1_000));
    assert_eq!(fresh.next_check_in_ms(1_000), Some(0));
    assert_eq!(fresh.next_check_in_ms(3_000), Some(0));

    let checked = monitor(MonitorStatus::Watching, 1_000, Some(6_000));
    assert_eq!(checked.next_check_at_ms(), Some(11_000));
    assert_eq!(checked.next_check_in_ms(6_000), Some(5_000));
    // the poll slipped: due immediately, never negative
    assert_eq!(checked.next_check_in_ms(20_000), Some(0));
}

#[test]
fn monitor_next_check_is_none_once_terminal() {
    for status in [
        MonitorStatus::Fired,
        MonitorStatus::Stopped,
        MonitorStatus::Failed,
    ] {
        assert_eq!(monitor(status, 1_000, None).next_check_in_ms(1_000), None);
    }
}

#[test]
fn monitor_poll_interval_floor_and_default() {
    assert_eq!(MonitorInfo::normalize_poll_interval(None), 5);
    assert_eq!(MonitorInfo::normalize_poll_interval(Some(0)), 5);
    assert_eq!(MonitorInfo::normalize_poll_interval(Some(1)), 1);
    assert_eq!(MonitorInfo::normalize_poll_interval(Some(30)), 30);
}

#[test]
fn monitor_summary_line_carries_status_and_condition() {
    let line = monitor(MonitorStatus::Watching, 0, None).summary_line();
    assert!(line.contains("mon_1"), "{line}");
    assert!(line.contains("[watching]"), "{line}");
    assert!(line.contains("build finished"), "{line}");
    assert!(line.contains("/tmp/done exists"), "{line}");
}

#[test]
fn an_interval_schedule_counts_from_the_previous_run() {
    let s = CronSchedule::every(90);
    assert_eq!(s.next_run_ms(1_000_000), Ok(1_090_000));
    // A zero interval is clamped to one second rather than firing forever.
    assert_eq!(CronSchedule::every(0).next_run_ms(0), Ok(1000));
}

#[test]
fn an_expression_schedule_is_evaluated_in_utc_and_validated() {
    let s = CronSchedule::cron("0 0 * * *");
    assert!(s.validate().is_ok());
    let midnight = s.next_run_ms(0).expect("next run");
    assert_eq!(midnight, 86_400_000);

    let bad = CronSchedule::cron("nope");
    assert!(bad.validate().is_err());
    assert!(bad.next_run_ms(0).is_err());
}

#[test]
fn rescheduling_respects_enabled_and_the_last_run() {
    let mut job = CronJob {
        id: pacode_types_id(),
        name: "nightly".to_string(),
        schedule: CronSchedule::every(60),
        prompt: "run the suite".to_string(),
        enabled: true,
        created_at_ms: 0,
        last_run_ms: Some(10_000),
        next_run_ms: None,
        last_status: None,
    };
    job.reschedule(10_000).expect("reschedule");
    assert_eq!(job.next_run_ms, Some(70_000));

    job.enabled = false;
    job.reschedule(10_000).expect("reschedule");
    assert_eq!(job.next_run_ms, None);
    assert_eq!(job.due_in_ms(10_000), None);
}

fn pacode_types_id() -> crate::ids::CronJobId {
    crate::ids::CronJobId::generate()
}
