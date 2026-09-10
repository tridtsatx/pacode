use super::*;
use crate::state::IDLE_DEBOUNCE_MS;
use std::time::Duration;

#[test]
fn test_rail_idle_debounce() {
    let mut rail = RailState::default();
    let t0 = Instant::now();

    // Turn active -> not idle
    assert!(!rail.update_idle(true, t0));
    assert!(!rail.show_session_stats);
    assert!(rail.idle_since.is_none());

    // Turn finishes at t1
    let t1 = t0 + Duration::from_millis(100);
    assert!(!rail.update_idle(false, t1));
    assert!(!rail.show_session_stats);
    assert_eq!(rail.idle_since, Some(t1));

    // After 1 second (less than 2s debounce)
    let t2 = t1 + Duration::from_millis(1000);
    assert!(!rail.update_idle(false, t2));
    assert!(!rail.show_session_stats);

    // After 2.1 seconds (> 2s debounce)
    let t3 = t1 + Duration::from_millis(IDLE_DEBOUNCE_MS + 100);
    assert!(rail.update_idle(false, t3));
    assert!(rail.show_session_stats);

    // Subsequent idle call does not trigger change again
    assert!(!rail.update_idle(false, t3 + Duration::from_millis(100)));

    // New turn starts -> immediately reverts to AGENTS
    let t4 = t3 + Duration::from_millis(200);
    assert!(rail.update_idle(true, t4));
    assert!(!rail.show_session_stats);
    assert!(rail.idle_since.is_none());
}

#[test]
fn the_idle_debounce_keeps_the_tick_armed_until_it_completes() {
    use std::time::Duration;

    let mut rail = RailState::default();
    let start = Instant::now();

    // Going quiet starts the debounce; nothing is shown yet, but the tick must
    // stay armed or the block would never appear.
    assert!(!rail.update_idle(false, start));
    assert!(rail.idle_debounce_pending());
    assert_eq!(
        rail.idle_debounce_remaining_ms(start),
        Some(crate::state::IDLE_DEBOUNCE_MS)
    );

    let midway = start + Duration::from_millis(crate::state::IDLE_DEBOUNCE_MS / 2);
    assert!(!rail.update_idle(false, midway));
    assert!(rail.idle_debounce_pending());

    let due = start + Duration::from_millis(crate::state::IDLE_DEBOUNCE_MS);
    assert!(rail.update_idle(false, due));
    assert!(rail.show_session_stats);
    assert!(!rail.idle_debounce_pending());
    assert_eq!(rail.idle_debounce_remaining_ms(due), None);

    // A new turn hides the block again and re-arms nothing until it ends.
    assert!(rail.update_idle(true, due));
    assert!(!rail.show_session_stats);
    assert!(!rail.idle_debounce_pending());
}

#[test]
fn the_schedule_countdown_ticks_at_the_resolution_it_shows() {
    use pacode_types::{CronJob, CronJobId, CronSchedule};

    let mut rail = RailState::default();
    // Nothing scheduled: no tick at all.
    assert_eq!(rail.schedule_tick_ms(1_000), None);

    let mut job = CronJob {
        id: CronJobId::new("cron_1"),
        name: "nightly".to_string(),
        schedule: CronSchedule::every(3600),
        prompt: "x".to_string(),
        enabled: true,
        created_at_ms: 0,
        last_run_ms: None,
        next_run_ms: Some(100_000),
        last_status: None,
    };
    rail.upsert_cron_job(job.clone());

    // Far away: the rail shows whole minutes, so one wake a minute is enough.
    let tick = rail.schedule_tick_ms(0).expect("tick");
    assert!(tick <= 60_000, "a distant job must not tick every second");

    // Inside the last minute the countdown shows seconds.
    job.next_run_ms = Some(30_400);
    rail.upsert_cron_job(job.clone());
    let tick = rail.schedule_tick_ms(0).expect("tick");
    assert!(tick <= 1_000, "a near job ticks per second: {tick}");

    // A disabled job counts down to nothing and needs no tick.
    job.enabled = false;
    rail.upsert_cron_job(job);
    assert_eq!(rail.schedule_tick_ms(0), None);
}

#[test]
fn cron_jobs_and_monitors_are_upserted_by_id() {
    use pacode_types::{
        CronJob, CronJobId, CronSchedule, MonitorCondition, MonitorId, MonitorInfo, MonitorStatus,
    };

    let mut rail = RailState::default();
    let job = CronJob {
        id: CronJobId::new("cron_1"),
        name: "first".to_string(),
        schedule: CronSchedule::every(60),
        prompt: "x".to_string(),
        enabled: true,
        created_at_ms: 10,
        last_run_ms: None,
        next_run_ms: Some(70),
        last_status: None,
    };
    rail.upsert_cron_job(job.clone());
    let mut renamed = job.clone();
    renamed.name = "renamed".to_string();
    rail.upsert_cron_job(renamed);
    assert_eq!(rail.cron_jobs.len(), 1);
    assert_eq!(rail.cron_jobs[0].name, "renamed");
    assert!(rail.has_schedule_rows());

    rail.remove_cron_job(&CronJobId::new("cron_1"));
    assert!(rail.cron_jobs.is_empty());
    assert!(!rail.has_schedule_rows());

    let monitor = MonitorInfo {
        id: MonitorId::new("mon_1"),
        label: "build".to_string(),
        condition: MonitorCondition::FileExists {
            path: "done".to_string(),
        },
        poll_interval_secs: 5,
        started_at_ms: 0,
        status: MonitorStatus::Watching,
        last_check_ms: None,
        fired_at_ms: None,
    };
    rail.upsert_monitor(monitor.clone());
    assert!(rail.has_schedule_rows());

    let mut fired = monitor;
    fired.status = MonitorStatus::Fired;
    rail.upsert_monitor(fired);
    assert_eq!(rail.monitors.len(), 1);
    // A monitor that fired is no longer waiting for anything.
    assert!(!rail.has_schedule_rows());
}
