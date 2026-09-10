use std::path::PathBuf;

use pacode_types::{CronSchedule, MonitorCondition, MonitorStatus};

use super::*;

const NOW: u64 = 1_700_000_000_000;

fn scheduler() -> Arc<Scheduler> {
    Scheduler::new()
}

#[test]
fn an_added_job_is_scheduled_and_listed() {
    let s = scheduler();
    let job = s
        .add_job(
            "nightly".to_string(),
            CronSchedule::every(600),
            "run the suite".to_string(),
            NOW,
        )
        .expect("add");

    assert_eq!(job.next_run_ms, Some(NOW + 600_000));
    assert_eq!(s.jobs().len(), 1);
    assert_eq!(s.next_deadline_ms(NOW), Some(NOW + 600_000));
}

#[test]
fn an_invalid_expression_is_refused_instead_of_scheduling_nothing() {
    let s = scheduler();
    let err = s
        .add_job(
            "broken".to_string(),
            CronSchedule::cron("not a cron"),
            "x".to_string(),
            NOW,
        )
        .expect_err("must be refused");
    assert!(matches!(err, ScheduleError::Schedule(_)));
    assert!(s.jobs().is_empty());
}

#[test]
fn the_job_cap_is_enforced() {
    let s = scheduler();
    for i in 0..MAX_CRON_JOBS {
        s.add_job(
            format!("job {i}"),
            CronSchedule::every(60),
            "x".to_string(),
            NOW,
        )
        .expect("add");
    }
    let err = s
        .add_job(
            "one too many".to_string(),
            CronSchedule::every(60),
            "x".to_string(),
            NOW,
        )
        .expect_err("cap");
    assert!(matches!(err, ScheduleError::TooManyJobs));
    assert_eq!(s.jobs().len(), MAX_CRON_JOBS);
}

#[test]
fn a_disabled_job_has_no_deadline_and_does_not_come_due() {
    let s = scheduler();
    let job = s
        .add_job(
            "nightly".to_string(),
            CronSchedule::every(60),
            "x".to_string(),
            NOW,
        )
        .expect("add");

    s.set_enabled(&job.id, false, NOW).expect("disable");
    assert_eq!(s.next_deadline_ms(NOW), None);
    assert!(s.take_due_jobs(NOW + 3_600_000).is_empty());

    s.set_enabled(&job.id, true, NOW).expect("enable");
    assert_eq!(s.next_deadline_ms(NOW), Some(NOW + 60_000));
}

#[test]
fn a_due_job_is_taken_once_and_rescheduled() {
    let s = scheduler();
    s.add_job(
        "every minute".to_string(),
        CronSchedule::every(60),
        "x".to_string(),
        NOW,
    )
    .expect("add");

    let due = s.take_due_jobs(NOW + 60_000);
    assert_eq!(due.len(), 1);
    // Taking it moved the deadline on, so the same instant does not fire twice.
    assert!(s.take_due_jobs(NOW + 60_000).is_empty());
    assert_eq!(s.jobs()[0].last_run_ms, Some(NOW + 60_000));
    assert_eq!(s.jobs()[0].next_run_ms, Some(NOW + 120_000));
}

#[test]
fn run_now_brings_the_deadline_forward() {
    let s = scheduler();
    let job = s
        .add_job(
            "hourly".to_string(),
            CronSchedule::every(3600),
            "x".to_string(),
            NOW,
        )
        .expect("add");
    assert_eq!(s.next_deadline_ms(NOW), Some(NOW + 3_600_000));

    s.run_now(&job.id, NOW).expect("run now");
    assert_eq!(s.next_deadline_ms(NOW), Some(NOW));
    assert_eq!(s.take_due_jobs(NOW).len(), 1);
}

#[test]
fn removing_an_unknown_job_is_an_error_not_a_silent_success() {
    let s = scheduler();
    let id = pacode_types::CronJobId::new("cron_nope");
    assert!(matches!(
        s.remove_job(&id),
        Err(ScheduleError::UnknownJob(_))
    ));
    assert!(matches!(
        s.set_enabled(&id, true, NOW),
        Err(ScheduleError::UnknownJob(_))
    ));
    assert!(matches!(
        s.run_now(&id, NOW),
        Err(ScheduleError::UnknownJob(_))
    ));
}

#[test]
fn loaded_jobs_are_rescheduled_and_a_broken_one_is_disabled_rather_than_dropped() {
    let s = scheduler();
    let good = CronJob {
        id: pacode_types::CronJobId::new("cron_good"),
        name: "good".to_string(),
        schedule: CronSchedule::every(120),
        prompt: "x".to_string(),
        enabled: true,
        created_at_ms: 0,
        last_run_ms: None,
        next_run_ms: Some(1),
        last_status: None,
    };
    let broken = CronJob {
        id: pacode_types::CronJobId::new("cron_broken"),
        name: "broken".to_string(),
        schedule: CronSchedule::cron("99 * * * *"),
        prompt: "x".to_string(),
        enabled: true,
        created_at_ms: 0,
        last_run_ms: None,
        next_run_ms: Some(1),
        last_status: None,
    };
    s.load_jobs(vec![good, broken], NOW);

    let jobs = s.jobs();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].next_run_ms, Some(NOW + 120_000));
    assert!(!jobs[1].enabled);
    assert!(
        jobs[1]
            .last_status
            .as_deref()
            .is_some_and(|s| s.starts_with("disabled:")),
        "a job that cannot be scheduled must say why: {:?}",
        jobs[1].last_status
    );
}

#[test]
fn nothing_scheduled_means_no_deadline_at_all() {
    let s = scheduler();
    assert_eq!(s.next_deadline_ms(NOW), None);

    // A monitor that has stopped does not keep the scheduler awake either.
    let m = s
        .add_monitor(
            "build".to_string(),
            MonitorCondition::FileExists {
                path: "target/done".to_string(),
            },
            Some(5),
            NOW,
        )
        .expect("add monitor");
    assert!(s.next_deadline_ms(NOW).is_some());
    s.stop_monitor(&m.id).expect("stop");
    assert_eq!(s.next_deadline_ms(NOW), None);
}

#[test]
fn a_monitor_polls_at_its_interval_and_the_floor_is_one_second() {
    let s = scheduler();
    let m = s
        .add_monitor(
            "build".to_string(),
            MonitorCondition::FileExists {
                path: "target/done".to_string(),
            },
            Some(0),
            NOW,
        )
        .expect("add");
    assert!(m.poll_interval_secs >= MIN_POLL_SECS);

    // Never checked yet: due immediately.
    assert_eq!(s.due_monitors(NOW).len(), 1);
    s.record_check(&m.id, NOW, false);
    assert!(s.due_monitors(NOW).is_empty());
    assert_eq!(s.due_monitors(NOW + m.poll_interval_secs * 1000).len(), 1);
}

#[test]
fn a_fired_monitor_stops_polling() {
    let s = scheduler();
    let m = s
        .add_monitor(
            "build".to_string(),
            MonitorCondition::FileExists {
                path: "target/done".to_string(),
            },
            Some(1),
            NOW,
        )
        .expect("add");

    let fired = s.record_check(&m.id, NOW, true).expect("record");
    assert_eq!(fired.status, MonitorStatus::Fired);
    assert_eq!(fired.fired_at_ms, Some(NOW));
    assert!(s.due_monitors(NOW + 10_000).is_empty());
    assert_eq!(s.next_deadline_ms(NOW), None);
}

#[test]
fn the_monitor_cap_counts_live_monitors_only() {
    let s = scheduler();
    let mut ids = Vec::new();
    for i in 0..MAX_MONITORS {
        let m = s
            .add_monitor(
                format!("m{i}"),
                MonitorCondition::FileExists {
                    path: "x".to_string(),
                },
                Some(5),
                NOW,
            )
            .expect("add");
        ids.push(m.id);
    }
    assert!(matches!(
        s.add_monitor(
            "one too many".to_string(),
            MonitorCondition::FileExists {
                path: "x".to_string()
            },
            Some(5),
            NOW,
        ),
        Err(ScheduleError::TooManyMonitors)
    ));

    // Stopping one frees a slot: the cap is about what is still watching.
    s.stop_monitor(&ids[0]).expect("stop");
    s.add_monitor(
        "now there is room".to_string(),
        MonitorCondition::FileExists {
            path: "x".to_string(),
        },
        Some(5),
        NOW,
    )
    .expect("add after freeing a slot");
}

#[tokio::test]
async fn file_conditions_are_checked_against_the_session_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_path_buf();

    let exists = MonitorCondition::FileExists {
        path: "marker".to_string(),
    };
    assert!(!check_condition(&exists, &cwd).await.expect("check"));
    std::fs::write(cwd.join("marker"), b"ready\n").expect("write");
    assert!(check_condition(&exists, &cwd).await.expect("check"));

    let matches = MonitorCondition::FileMatches {
        path: "marker".to_string(),
        pattern: "ready".to_string(),
    };
    assert!(check_condition(&matches, &cwd).await.expect("check"));

    let no_match = MonitorCondition::FileMatches {
        path: "marker".to_string(),
        pattern: "failed".to_string(),
    };
    assert!(!check_condition(&no_match, &cwd).await.expect("check"));

    // A file that is not there has simply not matched yet; it is not an error.
    let missing = MonitorCondition::FileMatches {
        path: "absent".to_string(),
        pattern: "anything".to_string(),
    };
    assert!(!check_condition(&missing, &cwd).await.expect("check"));
}

#[tokio::test]
async fn a_command_condition_holds_when_the_command_succeeds() {
    let cwd = PathBuf::from(".");
    let ok = MonitorCondition::CommandSucceeds {
        command: "true".to_string(),
    };
    assert!(check_condition(&ok, &cwd).await.expect("check"));

    let fails = MonitorCondition::CommandSucceeds {
        command: "false".to_string(),
    };
    assert!(!check_condition(&fails, &cwd).await.expect("check"));
}

#[tokio::test]
async fn a_process_condition_holds_once_nothing_matches_the_pattern() {
    let cwd = PathBuf::from(".");
    let gone = MonitorCondition::ProcessGone {
        pattern: "pacode-no-such-process-xyzzy".to_string(),
    };
    assert!(check_condition(&gone, &cwd).await.expect("check"));
}
