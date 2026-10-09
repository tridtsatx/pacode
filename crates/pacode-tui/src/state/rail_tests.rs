use super::*;
use crate::state::IDLE_DEBOUNCE_MS;
use pacode_types::{AgentKind, AgentStatus, Effort, ModelRoute, TurnId, TurnStop};
use std::time::Duration;

fn agent(id: AgentId, status: AgentStatus) -> AgentInfo {
    let kind = if id.is_main() {
        AgentKind::Main
    } else {
        AgentKind::Sub
    };
    AgentInfo {
        id,
        name: "a".to_string(),
        kind,
        status,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("prov", "model"),
        effort: Effort::High,
        parent: (kind == AgentKind::Sub).then(AgentId::main),
        summary: None,
        error: None,
    }
}

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
fn needs_second_tick_stays_armed_until_the_idle_debounce_completes() {
    let mut state =
        crate::state::AppState::new(pacode_types::Config::default(), "test".to_string(), 120, 34);
    let start = Instant::now();
    assert!(!state.needs_second_tick());

    // Going quiet arms the debounce — the tick has to stay armed for the whole
    // window or the SESSION block would never appear on an otherwise still
    // screen.
    assert!(!state.rail.update_idle(false, start));
    assert!(state.rail.idle_debounce_pending());
    assert!(state.needs_second_tick());

    let midway = start + Duration::from_millis(IDLE_DEBOUNCE_MS / 2);
    assert!(!state.rail.update_idle(false, midway));
    assert!(state.needs_second_tick());

    let due = start + Duration::from_millis(IDLE_DEBOUNCE_MS);
    assert!(state.rail.update_idle(false, due));
    assert!(state.rail.show_session_stats);
    assert!(!state.needs_second_tick());
}

#[test]
fn an_idle_subagent_is_still_live() {
    let mut rail = RailState::default();
    assert!(!rail.has_live_agents());

    // A subagent sits in `Idle` between spawn and `TurnStarted` — live, so the
    // rail keeps ticking its duration and the screen never reads quiet.
    rail.upsert_agent(agent(AgentId::new("sub_1"), AgentStatus::Idle));
    assert!(rail.has_live_agents());

    let t0 = Instant::now();
    assert!(!rail.update_idle(false, t0));
    assert!(rail.idle_since.is_none());
    assert!(!rail.idle_debounce_pending());
}

#[test]
fn the_main_agents_own_row_is_not_a_live_agent() {
    // Main's activity is gated by `turn_active` at the call sites — a stale
    // `Thinking` row left after `TurnEnded` must not tick on its own.
    let mut rail = RailState::default();
    for status in [
        AgentStatus::Idle,
        AgentStatus::Thinking,
        AgentStatus::RunningTool,
        AgentStatus::WaitingApproval,
    ] {
        rail.upsert_agent(agent(AgentId::main(), status));
        assert!(!rail.has_live_agents(), "main {status:?} must not count");
    }
}

#[test]
fn a_terminal_subagent_is_not_live() {
    let mut rail = RailState::default();
    rail.upsert_agent(agent(AgentId::new("sub_1"), AgentStatus::RunningTool));
    assert!(rail.has_live_agents());

    for status in [
        AgentStatus::Finished,
        AgentStatus::Failed,
        AgentStatus::Stopped,
    ] {
        rail.upsert_agent(agent(AgentId::new("sub_1"), status));
        assert!(!rail.has_live_agents(), "{status:?} is terminal");
    }
}

#[test]
fn direct_agent_vector_edits_do_not_break_indexed_lookup() {
    let mut rail = RailState::default();
    let id = AgentId::new("sub_1");
    rail.agents
        .push(agent(id.clone(), AgentStatus::RunningTool));
    assert!(rail.agent(&id).is_some());

    rail.agents[0].id = AgentId::new("sub_2");
    assert!(rail.agent(&id).is_none());
    assert!(rail.agent(&AgentId::new("sub_2")).is_some());
}

#[test]
fn stale_agent_turn_end_cannot_finish_the_new_turn() {
    let mut rail = RailState::default();
    let id = AgentId::new("sub_1");
    let old_turn = TurnId::new("turn_old");
    let new_turn = TurnId::new("turn_new");
    rail.upsert_agent(agent(id.clone(), AgentStatus::RunningTool));
    rail.begin_agent_turn(&id, &old_turn);
    rail.begin_agent_turn(&id, &new_turn);

    assert_eq!(
        rail.finish_agent_turn(&id, &old_turn, &TurnStop::Completed, 1),
        TurnEndMatch::Stale
    );
    assert!(rail.agent(&id).is_some_and(|info| info.status.is_live()));
    assert_eq!(
        rail.finish_agent_turn(&id, &new_turn, &TurnStop::Completed, 2),
        TurnEndMatch::Current
    );
    assert_eq!(
        rail.agent(&id).map(|info| info.status),
        Some(AgentStatus::Finished)
    );
    assert_eq!(
        rail.finish_agent_turn(
            &id,
            &new_turn,
            &TurnStop::Failed {
                message: "late".into(),
            },
            3
        ),
        TurnEndMatch::Duplicate
    );
    assert_eq!(
        rail.agent(&id).map(|info| info.status),
        Some(AgentStatus::Finished)
    );
}

#[test]
fn the_schedule_countdown_ticks_at_the_resolution_it_shows() {
    use pacode_types::{CronJob, CronJobId, CronSchedule};

    let mut rail = RailState::default();
    // Nothing scheduled: no tick at all.
    assert_eq!(rail.schedule_tick_ms(1_000, usize::MAX), None);

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

    // Hours away: the rail shows whole minutes, so one wake a minute is enough.
    job.next_run_ms = Some(2 * 3_600_000 + 30_500);
    rail.upsert_cron_job(job.clone());
    let tick = rail.schedule_tick_ms(0, usize::MAX).expect("tick");
    assert!(
        tick > 1_000 && tick <= 60_000,
        "a distant job must not tick every second: {tick}"
    );

    // Inside the last minute the countdown shows seconds.
    job.next_run_ms = Some(30_400);
    rail.upsert_cron_job(job.clone());
    let tick = rail.schedule_tick_ms(0, usize::MAX).expect("tick");
    assert!(tick <= 1_000, "a near job ticks per second: {tick}");

    // A disabled job counts down to nothing and needs no tick.
    job.enabled = false;
    rail.upsert_cron_job(job);
    assert_eq!(rail.schedule_tick_ms(0, usize::MAX), None);
}

fn cron(id: &str, next_run_ms: u64) -> pacode_types::CronJob {
    pacode_types::CronJob {
        id: pacode_types::CronJobId::new(id),
        name: id.to_string(),
        schedule: pacode_types::CronSchedule::every(3600),
        prompt: "x".to_string(),
        enabled: true,
        created_at_ms: 0,
        last_run_ms: None,
        next_run_ms: Some(next_run_ms),
        last_status: None,
    }
}

/// B26: the wake lands exactly where the rail's `in …` text changes — not a
/// moment earlier (a wasted frame) and not later (a stale countdown). The
/// rail prints seconds below an hour (`1m40s`), so a 100 s job must not wait
/// for the minute.
#[test]
fn the_schedule_wake_lands_on_the_next_change_of_the_text() {
    use pacode_types::time::format_duration_ms;
    for due in [12_345, 100_000, 59_999, 3_599_999, 3_600_000, 7_230_500] {
        let mut rail = RailState::default();
        rail.upsert_cron_job(cron("j", due));
        let tick = rail.schedule_tick_ms(0, usize::MAX).expect("tick");
        let shown = format_duration_ms(due);
        assert_eq!(format_duration_ms(due - (tick - 1)), shown, "due {due}");
        assert_ne!(format_duration_ms(due - tick), shown, "due {due}");
    }
}

/// B26: the soonest change wins, which is not always the nearest job.
#[test]
fn the_schedule_wake_takes_the_soonest_change_over_all_jobs() {
    let mut rail = RailState::default();
    rail.upsert_cron_job(cron("near", 30_900));
    rail.upsert_cron_job(cron("far", 45_050));
    assert_eq!(rail.schedule_tick_ms(0, usize::MAX), Some(51));
}

/// B26: the event loop wakes for the countdown only while the rail is on
/// screen; at rest without one there is no timer.
#[test]
fn the_schedule_wake_is_armed_only_while_the_rail_shows() {
    let frame = |state: &mut crate::state::AppState| {
        let area = ratatui::layout::Rect::new(0, 0, state.cols, state.rows);
        crate::ui::prepare_frame(area, state, std::time::Instant::now());
    };
    let mut state =
        crate::state::AppState::new(pacode_types::Config::default(), "t".into(), 120, 34);
    frame(&mut state);
    assert_eq!(state.schedule_redraw_in(0), None);
    state.rail.upsert_cron_job(cron("j", 30_900));
    // Not drawn yet: the wakeup follows what is on screen.
    assert_eq!(state.schedule_redraw_in(0), None);
    frame(&mut state);
    assert_eq!(
        state.schedule_redraw_in(0),
        Some(Duration::from_millis(901))
    );
    // No rail at this width: nothing on screen counts down.
    state.cols = 60;
    frame(&mut state);
    assert_eq!(state.schedule_redraw_in(0), None);
}

/// B26: the SCHEDULE block needs its header plus a row; a zone the task list
/// fills, or one too short, draws no countdown — and so arms no wakeup.
#[test]
fn the_background_fit_draws_schedule_rows_only_where_they_fit() {
    use crate::ui::rail_session::background_fit;
    // The zone the layout sizes for header + rows shows every row.
    assert_eq!(background_fit(2, 0, 1).schedule_shown, Some(1));
    assert_eq!(background_fit(5, 0, 4).schedule_shown, Some(4));
    // More rows than fit: one gives way to `and N more`.
    assert_eq!(background_fit(5, 0, 6).schedule_shown, Some(3));
    // Too short: only the header and `and N more` fit — no countdown.
    assert_eq!(background_fit(2, 0, 3).schedule_shown, Some(0));
    assert_eq!(background_fit(1, 0, 1).schedule_shown, None);
    // Four tasks under their header fill a five-row zone.
    let fit = background_fit(5, 4, 1);
    assert_eq!((fit.tasks_shown, fit.tasks_more), (4, false));
    assert_eq!(fit.schedule_shown, None);
    // Too many tasks: the zone ends in `and N more`, no SCHEDULE.
    let fit = background_fit(5, 9, 1);
    assert_eq!((fit.tasks_shown, fit.tasks_more), (3, true));
    assert_eq!(fit.schedule_shown, None);
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

#[test]
fn a_row_lookup_survives_a_list_changed_behind_the_index() {
    let mut rail = RailState::default();
    let first = AgentId::new("agt_first");
    let second = AgentId::new("agt_second");
    rail.upsert_agent(agent(first.clone(), AgentStatus::RunningTool));
    rail.upsert_agent(agent(second.clone(), AgentStatus::RunningTool));

    // A snapshot replaces the list without going through the index: the same
    // length, different rows. A lookup that trusted the old positions would
    // answer with the wrong agent.
    rail.agents = vec![
        agent(AgentId::new("agt_other"), AgentStatus::Finished),
        agent(first.clone(), AgentStatus::Finished),
    ];
    assert_eq!(
        rail.agent(&first).map(|agent| agent.id.clone()),
        Some(first)
    );
    assert!(rail.agent(&second).is_none());
    assert!(rail.agent(&AgentId::new("agt_missing")).is_none());
}

#[test]
fn a_lookup_for_a_row_the_rail_does_not_list_keeps_the_index_usable() {
    let mut rail = RailState::default();
    let known = AgentId::new("agt_known");
    rail.upsert_agent(agent(known.clone(), AgentStatus::RunningTool));
    let version = rail.agents_version;
    let index_version = rail.agent_index_version;
    assert_eq!(version, index_version);

    for _ in 0..4 {
        assert!(rail.agent(&AgentId::new("agt_gone")).is_none());
    }
    assert_eq!(
        rail.agent_index_version, index_version,
        "a miss must not invalidate the index it just proved current"
    );
    assert_eq!(
        rail.agent(&known).map(|agent| agent.id.clone()),
        Some(known)
    );
}

#[test]
fn a_newer_row_still_updates_in_place_after_an_upsert() {
    let mut rail = RailState::default();
    let id = AgentId::new("agt_first");
    rail.upsert_agent(agent(id.clone(), AgentStatus::RunningTool));
    let mut done = agent(id.clone(), AgentStatus::Finished);
    done.finished_at_ms = Some(42);
    rail.upsert_agent(done);

    assert_eq!(rail.agents.len(), 1);
    assert_eq!(
        rail.agent(&id).map(|agent| agent.status),
        Some(AgentStatus::Finished)
    );
    assert_eq!(
        rail.agent(&id).and_then(|agent| agent.finished_at_ms),
        Some(42)
    );
}
