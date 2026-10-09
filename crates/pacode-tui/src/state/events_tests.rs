//! `apply_client_event` seq-gap resync tracking and the `TurnEnded` rail
//! fail-safe for a dropped terminal `AgentUpdated`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use pacode_client::{ClientEvent, ClientEventContext};
use pacode_types::ids::{AgentId, SessionId};
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{AgentKind, AgentStatus, Mode};
use pacode_types::{
    AgentInfo, Config, Event, Plan, Request, SessionMeta, SessionSnapshot, SessionStatusKind,
    ToastLevel, TranscriptItem, TranscriptKind, TurnId, TurnStop, UsageTotals,
};

use crate::state::{AppState, Connection, Focus, PanelTarget, RESYNC_DEBOUNCE_MS, SessionSlot};

fn meta() -> SessionMeta {
    SessionMeta {
        id: SessionId::new("ses_test"),
        name: Some("test".into()),
        cwd: PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        model: ModelRoute::new("prov", "model"),
        effort: Effort::High,
        mode: Mode::Build,
        first_prompt: None,
        zdr: false,
    }
}

/// A session-attached, connected state — the only shape where a seq gap is
/// allowed to arm a resync.
fn attached_state() -> AppState {
    let mut state = AppState::new(Config::default(), "1.0.0".to_string(), 120, 34);
    state.meta = Some(meta());
    state.connection = Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state
}

fn event_context() -> ClientEventContext {
    ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    }
}

fn snapshot(seq: u64, cwd: PathBuf) -> SessionSnapshot {
    let mut meta = meta();
    meta.cwd = cwd;
    SessionSnapshot {
        meta,
        agents: vec![],
        plan: Plan::default(),
        tasks: vec![],
        usage: UsageTotals::default(),
        transcript: vec![],
        has_more_history: false,
        pending_permissions: vec![],
        pending_questions: vec![],
        turn_active: false,
        active_turn: None,
        terminal_turn: None,
        terminal_error: None,
        cron_jobs: vec![],
        monitors: vec![],
        seq,
    }
}

fn ev(seq: u64) -> ClientEvent {
    ClientEvent::Event {
        context: event_context(),
        seq,
        event: Event::PlanUpdated(Plan::default()),
    }
}

fn sub_agent(id: &str, status: AgentStatus) -> AgentInfo {
    AgentInfo {
        id: AgentId::new(id),
        name: id.to_string(),
        kind: AgentKind::Sub,
        status,
        activity: Some("working".into()),
        started_at_ms: 1000,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("prov", "model"),
        effort: Effort::High,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    }
}

fn turn_ended(id: &AgentId, stop: TurnStop) -> ClientEvent {
    ClientEvent::Event {
        context: event_context(),
        seq: 1,
        event: Event::TurnEnded {
            agent: id.clone(),
            turn: TurnId::new("turn_1"),
            usage: None,
            stop,
        },
    }
}

fn resync_marker() -> ClientEvent {
    ClientEvent::ResyncNeeded {
        context: event_context(),
    }
}

#[test]
fn wire_resync_marker_arms_the_existing_snapshot_request() {
    let mut state = attached_state();
    let now = Instant::now();

    assert!(state.apply_client_event(resync_marker(), now));
    assert!(state.resync_want);
    assert_eq!(state.take_resync_request(now), Some(Request::GetSnapshot));
    assert!(state.resync_in_flight);
}

#[test]
fn reconnect_snapshot_marker_does_not_duplicate_recovery() {
    let mut state = attached_state();
    let now = Instant::now();
    let reconnect_context = ClientEventContext {
        connection_epoch: 2,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert!(state.apply_client_event(
        ClientEvent::Connected {
            context: reconnect_context.clone(),
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        now,
    ));
    assert!(!state.apply_client_event(
        ClientEvent::ResyncNeeded {
            context: reconnect_context.clone(),
        },
        now,
    ));
    assert!(!state.resync_want);

    assert!(state.apply_client_event(
        ClientEvent::Snapshot {
            context: reconnect_context,
            snapshot: snapshot(10, PathBuf::from("/tmp")),
        },
        now,
    ));
    assert!(state.take_resync_request(now).is_none());
}

#[test]
fn marker_during_resync_remains_wanted_after_snapshot_barrier() {
    let mut state = attached_state();
    let t0 = Instant::now();

    assert!(state.apply_client_event(resync_marker(), t0));
    assert_eq!(state.take_resync_request(t0), Some(Request::GetSnapshot));
    assert!(state.apply_client_event(resync_marker(), t0));

    assert!(state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snapshot(2, PathBuf::from("/tmp")),
        },
        t0,
    ));
    assert!(state.resync_want);
    assert_eq!(
        state.take_resync_request(t0 + Duration::from_millis(RESYNC_DEBOUNCE_MS)),
        Some(Request::GetSnapshot)
    );
}

#[test]
fn seq_gap_arms_one_debounced_resync() {
    let mut state = attached_state();
    let now = Instant::now();

    // Contiguous start: no want.
    state.apply_client_event(ev(1), now);
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, Some(1));

    // A dropped event: seq jumps 1 → 3.
    state.apply_client_event(ev(3), now);
    assert!(state.resync_want);

    // The drain yields exactly one GetSnapshot and marks it in flight.
    assert_eq!(state.take_resync_request(now), Some(Request::GetSnapshot));
    assert!(state.resync_in_flight);
    assert!(!state.resync_want);

    // A second gap while in flight does not send again.
    state.apply_client_event(ev(7), now);
    assert!(state.resync_want);
    assert!(state.take_resync_request(now).is_none());
    assert!(state.resync_want); // still armed behind the in-flight request
}

#[test]
fn snapshot_rebases_seq_and_settles_resync() {
    let mut state = attached_state();
    let now = Instant::now();
    let cwd = state.cwd();

    state.apply_client_event(ev(1), now);
    state.apply_client_event(ev(3), now);
    assert_eq!(state.take_resync_request(now), Some(Request::GetSnapshot));

    // The answering snapshot rebases the tracker on its own seq — without
    // the rebase the first post-snapshot event would look like a fresh gap
    // and resync forever.
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snapshot(7, cwd),
        },
        now,
    );
    assert!(!state.resync_in_flight);
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, Some(7));

    // Contiguous seqs after the snapshot do not re-arm.
    state.apply_client_event(ev(8), now);
    assert!(!state.resync_want);
    assert!(state.take_resync_request(now).is_none());
}

#[test]
fn stale_seq_at_or_before_snapshot_is_ignored_without_gap() {
    let mut state = attached_state();
    let now = Instant::now();
    let cwd = state.cwd();
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snapshot(10, cwd),
        },
        now,
    );

    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 9,
            event: Event::Toast {
                level: ToastLevel::Info,
                title: "stale".into(),
                detail: None,
            },
        },
        now,
    ));
    assert!(state.toasts.is_empty());
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, Some(10));
}

#[test]
fn forward_gap_arms_resync_without_later_rollback() {
    let mut state = attached_state();
    let now = Instant::now();
    let cwd = state.cwd();
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snapshot(10, cwd),
        },
        now,
    );

    assert!(state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 12,
            event: Event::Toast {
                level: ToastLevel::Info,
                title: "newer".into(),
                detail: None,
            },
        },
        now,
    ));
    assert!(state.resync_want);
    assert_eq!(state.last_event_seq, Some(12));

    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 11,
            event: Event::Toast {
                level: ToastLevel::Info,
                title: "late".into(),
                detail: None,
            },
        },
        now,
    ));
    assert_eq!(state.toasts.len(), 1);
    assert_eq!(state.last_event_seq, Some(12));
    let stale_cwd = state.cwd();
    assert!(!state.apply_snapshot(snapshot(11, stale_cwd), now));
    assert_eq!(state.toasts.len(), 1);
    assert_eq!(state.last_event_seq, Some(12));
}

#[test]
fn reconnect_snapshot_can_rebase_a_restarted_daemon_seq_space() {
    let mut state = attached_state();
    let now = Instant::now();
    let cwd = state.cwd();
    state.last_event_seq = Some(20);
    let context = ClientEventContext {
        connection_epoch: 2,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert!(state.apply_client_event(
        ClientEvent::Connected {
            context: context.clone(),
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        now,
    ));
    assert!(state.apply_client_event(
        ClientEvent::Snapshot {
            context,
            snapshot: snapshot(5, cwd),
        },
        now,
    ));
    assert_eq!(state.last_event_seq, Some(5));
    assert!(state.apply_client_event(
        ClientEvent::Event {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            seq: 6,
            event: Event::PlanUpdated(Plan::default()),
        },
        now,
    ));
    assert!(!state.resync_want);
}

#[test]
fn a_new_connection_heals_a_stale_resync_barrier() {
    let mut state = attached_state();
    state.resync_want = true;
    state.resync_in_flight = true;
    state.resync_sent_at = Some(Instant::now());
    state.awaiting_snapshot = true;
    assert!(state.apply_client_event(
        ClientEvent::Connected {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        Instant::now(),
    ));
    assert!(!state.resync_in_flight);
    assert!(!state.resync_want);
    assert!(state.resync_sent_at.is_none());
}

#[test]
fn pre_snapshot_events_replay_once_in_seq_order() {
    let mut state = attached_state();
    let now = Instant::now();
    let cwd = state.cwd();
    state.awaiting_snapshot = true;

    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 11,
            event: Event::Toast {
                level: ToastLevel::Info,
                title: "live".into(),
                detail: None,
            },
        },
        now,
    ));
    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 9,
            event: Event::Toast {
                level: ToastLevel::Info,
                title: "covered".into(),
                detail: None,
            },
        },
        now,
    ));
    assert_eq!(state.session.buffered_event_count(), 2);
    assert!(state.toasts.is_empty());

    assert!(state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snapshot(10, cwd),
        },
        now,
    ));
    assert_eq!(state.session.buffered_event_count(), 0);
    assert_eq!(state.last_event_seq, Some(11));
    assert_eq!(state.toasts.len(), 1);
    assert_eq!(state.toasts[0].title, "live");
    assert!(!state.resync_want);

    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 11,
            event: Event::Toast {
                level: ToastLevel::Info,
                title: "live".into(),
                detail: None,
            },
        },
        now,
    ));
    assert_eq!(state.toasts.len(), 1);
}

#[test]
fn resync_debounce_paces_resend() {
    let mut state = attached_state();
    let t0 = Instant::now();

    state.apply_client_event(ev(1), t0);
    state.apply_client_event(ev(3), t0);
    assert_eq!(state.take_resync_request(t0), Some(Request::GetSnapshot));

    // The request failed; the app clears in-flight and re-arms the want.
    state.resync_in_flight = false;
    state.resync_want = true;

    // Inside the debounce window the want waits.
    let inside = t0 + Duration::from_millis(RESYNC_DEBOUNCE_MS - 1);
    assert!(state.take_resync_request(inside).is_none());
    assert!(state.resync_want);

    // Past it the retry goes out.
    let past = t0 + Duration::from_millis(RESYNC_DEBOUNCE_MS);
    assert_eq!(state.take_resync_request(past), Some(Request::GetSnapshot));
}

#[test]
fn resync_want_drops_when_detached_or_disconnected() {
    let now = Instant::now();

    // No session attached: the attach snapshot supersedes any resync.
    let mut state = attached_state();
    state.apply_client_event(ev(1), now);
    state.apply_client_event(ev(3), now);
    assert!(state.resync_want);
    state.meta = None;
    assert!(state.take_resync_request(now).is_none());
    assert!(!state.resync_want);

    // Link down: the reconnect's own snapshot supersedes it.
    let mut state = attached_state();
    state.apply_client_event(ev(1), now);
    state.apply_client_event(ev(3), now);
    state.connection = Connection::Reconnecting { attempt: 2 };
    assert!(state.take_resync_request(now).is_none());
    assert!(!state.resync_want);
}

#[test]
fn seq_gap_ignored_before_attach() {
    // `meta` empty — the incoming attach snapshot is authoritative anyway.
    let mut state = AppState::new(Config::default(), "1.0.0".to_string(), 120, 34);
    let now = Instant::now();
    state.apply_client_event(ev(1), now);
    state.apply_client_event(ev(9), now);
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, None);
}

#[test]
fn turn_ended_failsafe_marks_live_rail_agent_terminal() {
    for (stop, want) in [
        (TurnStop::Completed, AgentStatus::Finished),
        (TurnStop::Interrupted, AgentStatus::Stopped),
        (
            TurnStop::Failed {
                message: "boom".into(),
            },
            AgentStatus::Failed,
        ),
    ] {
        let mut state = attached_state();
        let id = AgentId::new("agt_sub1");
        state
            .rail
            .upsert_agent(sub_agent("agt_sub1", AgentStatus::RunningTool));
        state.apply_client_event(turn_ended(&id, stop), Instant::now());
        let info = state.rail.agent(&id).unwrap();
        assert_eq!(info.status, want, "stop variant");
        assert!(info.finished_at_ms.is_some());
    }
}

#[test]
fn turn_ended_failsafe_keeps_terminal_entry_and_stamp() {
    let mut state = attached_state();
    let id = AgentId::new("agt_sub1");
    let mut agent = sub_agent("agt_sub1", AgentStatus::Finished);
    agent.finished_at_ms = Some(42);
    state.rail.upsert_agent(agent);
    state.apply_client_event(
        turn_ended(
            &id,
            TurnStop::Failed {
                message: "boom".into(),
            },
        ),
        Instant::now(),
    );
    let info = state.rail.agent(&id).unwrap();
    assert_eq!(info.status, AgentStatus::Finished);
    assert_eq!(info.finished_at_ms, Some(42));
}

#[test]
fn turn_ended_failsafe_ignores_unknown_agent() {
    let mut state = attached_state();
    let before = state.rail.agents.len();
    state.apply_client_event(
        turn_ended(&AgentId::new("agt_ghost"), TurnStop::Completed),
        Instant::now(),
    );
    assert_eq!(state.rail.agents.len(), before);
}

#[test]
fn turn_ended_main_does_not_touch_rail() {
    let mut state = attached_state();
    let mut main = sub_agent("agt_main", AgentStatus::Thinking);
    main.kind = AgentKind::Main;
    main.parent = None;
    let id = AgentId::main();
    main.id = id.clone();
    state.rail.upsert_agent(main);
    state.turn_active = true;

    state.apply_client_event(turn_ended(&id, TurnStop::Completed), Instant::now());

    // The main-agent arm handled the turn; the fail-safe is sub-agent only.
    assert!(!state.turn_active);
    let info = state.rail.agent(&id).unwrap();
    assert_eq!(info.status, AgentStatus::Thinking);
    assert!(info.finished_at_ms.is_none());
}

#[test]
fn unselected_agent_deltas_are_filtered_without_forcing_a_resync() {
    let mut state = attached_state();
    let id = AgentId::new("agt_sub1");
    state.awaiting_snapshot = true;
    let now = Instant::now();
    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 1,
            event: Event::TextDelta {
                agent: id.clone(),
                item_seq: 1,
                text: "hidden".into(),
            },
        },
        now,
    ));
    assert_eq!(state.session.buffered_event_count(), 1);
    assert!(state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snapshot(1, PathBuf::from("/tmp")),
        },
        now,
    ));
    assert!(!state.resync_want);
    assert!(state.panel.agent_transcript.cells.is_empty());
}

#[test]
fn selected_agent_deltas_survive_the_snapshot_barrier() {
    let mut state = attached_state();
    let id = AgentId::new("agt_sub1");
    state.awaiting_snapshot = true;
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(id.clone()),
        follow: false,
        follow_paused: false,
    };
    let now = Instant::now();
    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 1,
            event: Event::ItemAdded(TranscriptItem {
                seq: 1,
                agent: id.clone(),
                ts_ms: 1,
                kind: TranscriptKind::Assistant {
                    text: String::new(),
                    complete: false,
                },
            }),
        },
        now,
    ));
    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 2,
            event: Event::TextDelta {
                agent: id,
                item_seq: 1,
                text: "visible".into(),
            },
        },
        now,
    ));
    assert!(state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snapshot(0, PathBuf::from("/tmp")),
        },
        now,
    ));
    assert!(!state.resync_want);
    assert!(!state.panel.agent_transcript.cells.is_empty());
}

#[test]
fn a_stale_turn_end_cannot_finish_a_new_agent_turn() {
    let mut state = attached_state();
    let id = AgentId::new("agt_sub1");
    let old_turn = TurnId::new("turn_old");
    let new_turn = TurnId::new("turn_new");
    state
        .rail
        .upsert_agent(sub_agent("agt_sub1", AgentStatus::RunningTool));
    let now = Instant::now();
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 1,
            event: Event::TurnStarted {
                agent: id.clone(),
                turn: old_turn.clone(),
            },
        },
        now,
    );
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 2,
            event: Event::TurnStarted {
                agent: id.clone(),
                turn: new_turn.clone(),
            },
        },
        now,
    );
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 3,
            event: Event::TurnEnded {
                agent: id.clone(),
                turn: old_turn.clone(),
                usage: None,
                stop: TurnStop::Completed,
            },
        },
        now,
    );
    assert!(
        state
            .rail
            .agent(&id)
            .is_some_and(|info| info.status.is_live())
    );
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 4,
            event: Event::TurnEnded {
                agent: id,
                turn: new_turn,
                usage: None,
                stop: TurnStop::Completed,
            },
        },
        now,
    );
    assert_eq!(
        state
            .rail
            .agent(&AgentId::new("agt_sub1"))
            .map(|info| info.status),
        Some(AgentStatus::Finished)
    );
}

fn perm_request(id: &str) -> pacode_types::PermissionRequest {
    pacode_types::PermissionRequest {
        id: pacode_types::PermissionId::new(id),
        agent: AgentId::main(),
        agent_name: "main".into(),
        call_id: pacode_types::CallId::new("call_1"),
        tool: "bash".into(),
        title: "Bash: rm -rf /tmp/x".into(),
        detail: String::new(),
        risk: Some(pacode_types::RiskLevel::Confirm),
        created_at_ms: 1,
    }
}

fn perm_item(seq: u64, req: &pacode_types::PermissionRequest) -> pacode_types::TranscriptItem {
    pacode_types::TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: req.created_at_ms,
        kind: pacode_types::TranscriptKind::Permission(req.clone()),
    }
}

fn permission_cell_count(state: &AppState) -> usize {
    state
        .transcript
        .cells
        .iter()
        .filter(|c| {
            matches!(
                c.kind,
                crate::state::CellKind::Item(pacode_types::TranscriptKind::Permission(_))
            )
        })
        .count()
}

#[test]
fn permission_event_and_item_added_share_one_prompt_cell() {
    let mut state = attached_state();
    let now = Instant::now();
    let req = perm_request("perm_1");

    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 1,
            event: Event::PermissionRequested(req.clone()),
        },
        now,
    );
    // The daemon records the same request as a transcript item right after —
    // it must refresh the prompt, not render a second one.
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 2,
            event: Event::ItemAdded(perm_item(7, &req)),
        },
        now,
    );

    assert_eq!(permission_cell_count(&state), 1);
}

#[test]
fn snapshot_pending_permission_dedupes_against_the_tail_item() {
    let mut state = attached_state();
    let now = Instant::now();
    let req = perm_request("perm_9");
    let mut snap = snapshot(10, PathBuf::from("/tmp"));
    // A still-pending request sits in the transcript tail AND in
    // `pending_permissions` — both must land on one prompt.
    snap.transcript = vec![perm_item(9, &req)];
    snap.pending_permissions = vec![req];

    state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snap,
        },
        now,
    );

    assert_eq!(permission_cell_count(&state), 1);
}

#[test]
fn permission_explained_folds_into_the_pending_prompt() {
    let mut state = attached_state();
    let now = Instant::now();
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 1,
            event: Event::PermissionRequested(perm_request("perm_1")),
        },
        now,
    );
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 2,
            event: Event::PermissionExplained {
                permission: pacode_types::PermissionId::new("perm_1"),
                text: "deletes the tmp dir".into(),
                level: Some(pacode_types::ExplainLevel::High),
            },
        },
        now,
    );

    // The prompt is still the pending request — an explanation never resolves.
    let cell = state.transcript.cells.back().expect("prompt cell");
    assert!(matches!(
        cell.kind,
        crate::state::CellKind::Item(pacode_types::TranscriptKind::Permission(_))
    ));
    assert_eq!(
        cell.perm_explain,
        Some(crate::state::PermExplain::Done {
            text: "deletes the tmp dir".into(),
            level: Some(pacode_types::ExplainLevel::High),
        })
    );

    // A re-asked 'e' replaces the answer instead of stacking a second one.
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 3,
            event: Event::PermissionExplained {
                permission: pacode_types::PermissionId::new("perm_1"),
                text: "actually: removes one file".into(),
                level: Some(pacode_types::ExplainLevel::Low),
            },
        },
        now,
    );
    let cell = state.transcript.cells.back().expect("prompt cell");
    assert_eq!(
        cell.perm_explain,
        Some(crate::state::PermExplain::Done {
            text: "actually: removes one file".into(),
            level: Some(pacode_types::ExplainLevel::Low),
        })
    );
    assert_eq!(
        permission_cell_count(&state),
        1,
        "the prompt cell count must not grow"
    );
}

#[test]
fn permission_explained_after_resolve_lands_as_a_notice() {
    let mut state = attached_state();
    let now = Instant::now();
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 1,
            event: Event::PermissionRequested(perm_request("perm_1")),
        },
        now,
    );
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 2,
            event: Event::PermissionResolved {
                permission: pacode_types::PermissionId::new("perm_1"),
                decision: pacode_types::PermissionDecision::Deny,
                reason: Some("interrupted".into()),
            },
        },
        now,
    );

    // The verdict notice keeps the title and appends the reason.
    let cell = state.transcript.cells.back().expect("verdict cell");
    match &cell.kind {
        crate::state::CellKind::Item(pacode_types::TranscriptKind::Notice { text, .. }) => {
            assert_eq!(text, "Denied: Bash: rm -rf /tmp/x — interrupted");
        }
        other => panic!("expected a notice, got {other:?}"),
    }
    assert!(cell.perm_explain.is_none());

    // The aux call finished after the verdict — the answer still lands
    // rather than vanishing, and (it answers the user's `e`) in view.
    state.transcript.scroll_from_bottom = 4;
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 3,
            event: Event::PermissionExplained {
                permission: pacode_types::PermissionId::new("perm_1"),
                text: "was going to delete /tmp/x".into(),
                level: Some(pacode_types::ExplainLevel::Catastrophic),
            },
        },
        now,
    );
    let cell = state.transcript.cells.back().expect("explain notice");
    match &cell.kind {
        crate::state::CellKind::Item(pacode_types::TranscriptKind::Notice { text, .. }) => {
            assert_eq!(text, "explain · catastrophic: was going to delete /tmp/x");
        }
        other => panic!("expected a notice, got {other:?}"),
    }
    assert_eq!(state.transcript.scroll_from_bottom, 0);
}

#[test]
fn permission_explained_without_level_keeps_the_honest_text() {
    let mut state = attached_state();
    let now = Instant::now();
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 1,
            event: Event::PermissionRequested(perm_request("perm_1")),
        },
        now,
    );
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 2,
            event: Event::PermissionExplained {
                permission: pacode_types::PermissionId::new("perm_1"),
                text: "explain unavailable under zero-data-retention".into(),
                level: None,
            },
        },
        now,
    );
    let cell = state.transcript.cells.back().expect("prompt cell");
    assert_eq!(
        cell.perm_explain,
        Some(crate::state::PermExplain::Done {
            text: "explain unavailable under zero-data-retention".into(),
            level: None,
        })
    );
}

fn slot(session: &str, status: SessionStatusKind) -> SessionSlot {
    SessionSlot {
        id: SessionId::new(session),
        title: session.to_string(),
        turns: 0,
        context_tokens: 0,
        agents_count: 0,
        tasks_count: 0,
        status,
    }
}

#[test]
fn session_status_event_updates_the_named_slot() {
    let mut state = attached_state();
    let now = Instant::now();
    state.slots[1] = Some(slot("ses_b", SessionStatusKind::Idle));
    state.slots[2] = Some(slot("ses_c", SessionStatusKind::Idle));

    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 1,
            event: Event::SessionStatus {
                session: SessionId::new("ses_b"),
                status: SessionStatusKind::NeedsInput,
            },
        },
        now,
    );
    assert_eq!(
        state.slots[1].as_ref().unwrap().status,
        SessionStatusKind::NeedsInput
    );
    // Only the named slot moved.
    assert_eq!(
        state.slots[2].as_ref().unwrap().status,
        SessionStatusKind::Idle
    );

    // An id no slot holds is ignored.
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 2,
            event: Event::SessionStatus {
                session: SessionId::new("ses_elsewhere"),
                status: SessionStatusKind::Done,
            },
        },
        now,
    );
    assert_eq!(
        state.slots[1].as_ref().unwrap().status,
        SessionStatusKind::NeedsInput
    );
}

#[test]
fn session_status_seq_stays_out_of_session_tracking() {
    let mut state = attached_state();
    let now = Instant::now();
    state.slots[1] = Some(slot("ses_b", SessionStatusKind::Idle));
    state.last_event_seq = Some(7);

    // `SessionStatus` rides the daemon-wide stream — its seq belongs to the
    // global space and must never forge a session-stream gap.
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 999,
            event: Event::SessionStatus {
                session: SessionId::new("ses_b"),
                status: SessionStatusKind::Working,
            },
        },
        now,
    );
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, Some(7));

    // A real session event still tracks normally after it.
    state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 8,
            event: Event::PlanUpdated(Plan::default()),
        },
        now,
    );
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, Some(8));
}

#[test]
fn auth_update_seq_stays_out_of_session_tracking() {
    let mut state = attached_state();
    state.last_event_seq = Some(7);

    assert!(state.apply_client_event(
        ClientEvent::Event {
            context: event_context(),
            seq: 999,
            event: Event::AuthUpdated(pacode_types::ProviderAuthInfo {
                id: "github".into(),
                display_name: "GitHub".into(),
                auth_kind: "oauth".into(),
                detail: String::new(),
                recommended: true,
                state: pacode_types::AuthState::Configured,
                accounts: Vec::new(),
                active: None,
            }),
        },
        Instant::now(),
    ));
    assert_eq!(state.auth_providers.len(), 1);
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, Some(7));
}

#[test]
fn leaving_slot_seeds_its_live_status() {
    // Quiet on screen -> Idle (a `Done` would be wrong: it was just viewed).
    let mut state = attached_state();
    state.leave_active_slot();
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::Idle
    );

    // Running turn -> Working.
    let mut state = attached_state();
    state.turn_active = true;
    state.leave_active_slot();
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::Working
    );

    // Pending permission -> NeedsInput, and it outranks the running turn.
    let mut state = attached_state();
    state.turn_active = true;
    state.apply_event(
        1,
        Event::PermissionRequested(perm_request("perm_1")),
        Instant::now(),
    );
    state.leave_active_slot();
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::NeedsInput
    );

    // A live-but-idle subagent -> Working too: between spawn and
    // `TurnStarted` the subagent is work in flight, not a quiet session.
    let mut state = attached_state();
    state.apply_event(
        1,
        Event::AgentAdded(sub_agent("sub_1", AgentStatus::Idle)),
        Instant::now(),
    );
    state.leave_active_slot();
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::Working
    );
}

#[test]
fn attach_snapshot_reseeds_the_slot_status() {
    let mut state = attached_state();
    let now = Instant::now();
    state.slots[0] = Some(slot("ses_test", SessionStatusKind::Done));

    // A quiet attach clears `Done` — the session is being viewed.
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snapshot(5, PathBuf::from("/tmp")),
        },
        now,
    );
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::Idle
    );

    // A snapshot with a running turn seeds Working.
    let mut snap = snapshot(6, PathBuf::from("/tmp"));
    snap.turn_active = true;
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snap,
        },
        now,
    );
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::Working
    );

    // A snapshot with a pending permission seeds NeedsInput.
    let mut snap = snapshot(7, PathBuf::from("/tmp"));
    snap.pending_permissions = vec![perm_request("perm_9")];
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snap,
        },
        now,
    );
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::NeedsInput
    );

    // A live-but-idle subagent still seeds Working — the gap between spawn
    // and `TurnStarted` is work in flight.
    let mut snap = snapshot(8, PathBuf::from("/tmp"));
    snap.agents = vec![sub_agent("sub_1", AgentStatus::Idle)];
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snap,
        },
        now,
    );
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::Working
    );

    // The main agent's own row does not count — `turn_active` covers it.
    let mut snap = snapshot(9, PathBuf::from("/tmp"));
    let mut main = sub_agent("main", AgentStatus::Thinking);
    main.kind = AgentKind::Main;
    main.parent = None;
    snap.agents = vec![main];
    state.apply_client_event(
        ClientEvent::Snapshot {
            context: event_context(),
            snapshot: snap,
        },
        now,
    );
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::Idle
    );
}

#[test]
fn active_slot_status_tracks_asks_and_turn() {
    use pacode_types::SessionStatusKind as Kind;

    let mut state = attached_state();
    let now = Instant::now();
    assert_eq!(state.active_slot_status(), Kind::Idle);

    state.turn_active = true;
    assert_eq!(state.active_slot_status(), Kind::Working);

    // A pending permission outranks the running turn.
    state.apply_event(1, Event::PermissionRequested(perm_request("perm_1")), now);
    assert_eq!(state.active_slot_status(), Kind::NeedsInput);
    state.apply_event(
        2,
        Event::PermissionResolved {
            permission: pacode_types::PermissionId::new("perm_1"),
            decision: pacode_types::PermissionDecision::AllowOnce,
            reason: None,
        },
        now,
    );
    assert_eq!(state.active_slot_status(), Kind::Working);

    // An unanswered question reads the same.
    let question = pacode_types::Question::new(
        pacode_types::QuestionId::generate(),
        pacode_types::QuestionOrigin::new(
            AgentId::main(),
            "main",
            pacode_types::CallId::new("call_1"),
        ),
        "pick",
        "which?",
        vec![
            pacode_types::QuestionOption::new("a", ""),
            pacode_types::QuestionOption::new("b", ""),
        ],
        false,
        1,
    )
    .unwrap();
    let qid = question.id.clone();
    state.apply_event(3, Event::QuestionAsked(question), now);
    assert_eq!(state.active_slot_status(), Kind::NeedsInput);
    state.apply_event(
        4,
        Event::QuestionResolved {
            question: qid,
            answer: pacode_types::QuestionAnswer::choice(0),
        },
        now,
    );
    assert_eq!(state.active_slot_status(), Kind::Working);
}

#[test]
fn an_idle_subagent_reads_working_and_keeps_the_second_tick() {
    use pacode_types::SessionStatusKind as Kind;

    let mut state = attached_state();
    let now = Instant::now();
    assert_eq!(state.active_slot_status(), Kind::Idle);
    assert!(!state.needs_second_tick());

    // Between spawn and `TurnStarted` a subagent sits in `Idle` — still live:
    // the slot reads Working and the second tick stays armed so the rail
    // duration does not freeze.
    state.apply_event(
        1,
        Event::AgentAdded(sub_agent("sub_1", AgentStatus::Idle)),
        now,
    );
    assert_eq!(state.active_slot_status(), Kind::Working);
    assert!(state.needs_second_tick());

    // And it is not quiet: the session-stats debounce must not start.
    let turn_active = state.turn_active;
    assert!(!state.rail.update_idle(turn_active, now));
    assert!(state.rail.idle_since.is_none());
}

#[test]
fn stale_session_epoch_event_is_dropped_after_rebind() {
    let mut state = attached_state();
    state.event_connection_epoch = 3;
    state.event_session_epoch = 2;
    state.awaiting_snapshot = false;
    let context = ClientEventContext {
        connection_epoch: 3,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context,
            seq: 1,
            event: Event::Toast {
                level: ToastLevel::Info,
                title: "stale".into(),
                detail: None,
            },
        },
        Instant::now(),
    ));
    assert!(state.toasts.is_empty());
    state.event_connection_epoch = 4;
    let stale_connection = ClientEventContext {
        connection_epoch: 3,
        session_epoch: 2,
        session: Some(SessionId::new("ses_test")),
    };
    assert!(!state.apply_client_event(
        ClientEvent::Event {
            context: stale_connection,
            seq: 2,
            event: Event::Toast {
                level: ToastLevel::Info,
                title: "stale connection".into(),
                detail: None,
            },
        },
        Instant::now(),
    ));
    assert!(state.toasts.is_empty());
}

#[test]
fn login_progress_is_global_and_does_not_write_transcript() {
    let mut state = attached_state();
    state.event_connection_epoch = 3;
    state.event_session_epoch = 2;
    state.awaiting_snapshot = false;
    let context = ClientEventContext {
        connection_epoch: 3,
        session_epoch: 2,
        session: Some(SessionId::new("ses_test")),
    };
    assert!(state.apply_client_event(
        ClientEvent::Event {
            context,
            seq: 100,
            event: Event::LoginProgress {
                provider: "github".into(),
                stage: pacode_types::LoginStage::Waiting,
            },
        },
        Instant::now(),
    ));
    assert!(state.transcript.cells.is_empty());
    assert!(!state.toasts.is_empty());
}

#[test]
fn context_cleared_drops_everything_drawn_from_the_conversation() {
    let mut state = attached_state();
    let now = Instant::now();
    let mut snap = snapshot(1, PathBuf::from("/tmp"));
    snap.transcript = vec![TranscriptItem {
        seq: 1,
        agent: AgentId::main(),
        ts_ms: 1,
        kind: TranscriptKind::User {
            text: "before the clear".into(),
        },
    }];
    let mut main = sub_agent("agt_main", AgentStatus::Idle);
    main.id = AgentId::main();
    main.kind = AgentKind::Main;
    main.parent = None;
    snap.agents = vec![main, sub_agent("agt_sub1", AgentStatus::Thinking)];
    snap.plan = Plan {
        version: 1,
        items: vec![pacode_types::PlanItem {
            id: "p1".into(),
            content: "port the parser".into(),
            status: pacode_types::PlanStatus::Active,
            progress: None,
        }],
    };
    snap.usage.context_tokens = 5_000;
    snap.usage.turns = 4;
    assert!(state.apply_snapshot(snap, now));
    let sub = AgentId::new("agt_sub1");
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(sub.clone()),
        follow: true,
        follow_paused: false,
    };
    state.panel.target = Some(PanelTarget::Agent(sub));
    state.selection.active = true;
    assert!(!state.transcript.cells.is_empty());

    state.apply_event(2, Event::ContextCleared, now);

    assert!(state.transcript.cells.is_empty());
    assert!(!state.transcript.has_more_history);
    assert_eq!(state.transcript.scroll_from_bottom, 0);
    assert!(!state.selection.is_active());
    assert!(state.rail.plan.items.is_empty());
    assert_eq!(
        state
            .rail
            .agents
            .iter()
            .map(|agent| agent.id.clone())
            .collect::<Vec<_>>(),
        vec![AgentId::main()]
    );
    assert_eq!(state.rail.usage.context_tokens, 0);
    assert_eq!(state.rail.usage.turns, 4, "spend totals are not context");
    assert_eq!(state.panel.target, None);
    assert!(matches!(state.focus, Focus::Normal));
    assert_eq!(state.meta.as_ref().map(|m| m.id.as_str()), Some("ses_test"));
}

fn running_tool(seq: u64, agent: &AgentId) -> TranscriptItem {
    TranscriptItem {
        seq,
        agent: agent.clone(),
        ts_ms: seq,
        kind: TranscriptKind::ToolCall {
            call_id: pacode_types::CallId::new(format!("call_{seq}")),
            name: "bash".into(),
            title: "bash echo".into(),
            intent: None,
            status: pacode_types::ToolStatus::Running,
            preview: String::new(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    }
}

/// B05: an overlay opened over a subagent panel moves the keyboard to
/// `Focus::Overlay`, and closing it lands on `Focus::Normal` — the panel stays
/// on screen either way, so its agent's events must keep landing in it.
#[test]
fn a_panel_agent_keeps_streaming_while_the_keyboard_is_elsewhere() {
    let id = AgentId::new("agt_sub1");
    for focus in [
        Focus::Overlay(crate::state::Overlay::QuestionPicker),
        Focus::Normal,
    ] {
        let mut state = attached_state();
        state.panel.target = Some(PanelTarget::Agent(id.clone()));
        state.focus = focus;
        let now = Instant::now();
        crate::state::events::apply_event(
            &mut state,
            1,
            Event::ItemAdded(TranscriptItem {
                seq: 1,
                agent: id.clone(),
                ts_ms: 1,
                kind: TranscriptKind::Assistant {
                    text: String::new(),
                    complete: false,
                },
            }),
            now,
        );
        crate::state::events::apply_event(
            &mut state,
            2,
            Event::ItemAdded(running_tool(2, &id)),
            now,
        );
        crate::state::events::apply_event(
            &mut state,
            3,
            Event::ToolOutput {
                agent: id.clone(),
                item_seq: 2,
                text: "sub out".into(),
            },
            now,
        );
        assert_eq!(state.panel.agent_transcript.cells.len(), 2);
        assert_eq!(
            state
                .panel
                .agent_transcript
                .tool_streams
                .get(2)
                .map(|v| v.text),
            Some("sub out")
        );
        assert!(state.transcript.cells.is_empty());
    }
}

/// B06: item seqs are per agent, so a subagent's chunk for its seq N must not
/// grow the main agent's running row that happens to carry seq N too.
#[test]
fn a_subagent_tool_output_never_lands_in_the_main_running_row() {
    let mut state = attached_state();
    let sub = AgentId::new("agt_sub1");
    let now = Instant::now();
    crate::state::events::apply_event(
        &mut state,
        1,
        Event::ItemAdded(running_tool(5, &AgentId::main())),
        now,
    );
    crate::state::events::apply_event(
        &mut state,
        2,
        Event::ToolOutput {
            agent: sub,
            item_seq: 5,
            text: "sub out".into(),
        },
        now,
    );
    assert!(state.transcript.tool_streams.get(5).is_none());
    crate::state::events::apply_event(
        &mut state,
        3,
        Event::ToolOutput {
            agent: AgentId::main(),
            item_seq: 5,
            text: "main out".into(),
        },
        now,
    );
    assert_eq!(
        state.transcript.tool_streams.get(5).map(|v| v.text),
        Some("main out")
    );
}

fn session_updated(state: &mut AppState, meta: SessionMeta) {
    crate::state::events::apply_event(state, 1, Event::SessionUpdated(meta), Instant::now());
}

/// B21: folding a `SessionUpdated` does no disk IO on the UI thread — the
/// prefs file is untouched until the event loop hands the change off.
#[test]
fn session_updated_does_not_touch_prefs_on_the_ui_thread() {
    let root = std::env::temp_dir().join(format!("pacode-b21-{}", std::process::id()));
    let mut state = attached_state();
    state.paths = pacode_config::Paths::under(&root);
    let mut changed = meta();
    changed.effort = Effort::Low;
    changed.name = Some("renamed".into());
    session_updated(&mut state, changed);
    assert!(!pacode_config::prefs::prefs_file(&state.paths).exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// B21: only what this session changed moves the global defaults — a title
/// touch writes nothing, and an effort change does not drag the session's
/// model and mode along over defaults another session set.
#[test]
fn session_updated_hands_off_only_the_changed_defaults() {
    let mut state = attached_state();
    let mut renamed = meta();
    renamed.name = Some("renamed".into());
    session_updated(&mut state, renamed.clone());
    assert_eq!(state.take_prefs_write(), None);

    let mut changed = renamed;
    changed.effort = Effort::Low;
    session_updated(&mut state, changed.clone());
    assert_eq!(
        state.take_prefs_write(),
        Some(crate::state::PrefsDelta {
            effort: Some(Effort::Low),
            ..Default::default()
        })
    );

    // Another session's meta (a switch landing) is not a change.
    let mut other = changed;
    other.id = SessionId::new("ses_other");
    other.mode = Mode::Plan;
    session_updated(&mut state, other);
    assert_eq!(state.take_prefs_write(), None);
}

/// B21: two quick changes (shift+tab twice: Plan, then Auto) each hand a
/// write to the blocking pool, which may run them in either order. The file
/// must end on the later change either way.
#[test]
fn prefs_writes_land_in_change_order_whatever_order_the_writers_run() {
    for later_writer_first in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "pacode-b21-order-{}-{later_writer_first}",
            std::process::id()
        ));
        let paths = pacode_config::Paths::under(&root);
        let outbox = crate::state::PrefsOutbox::default();
        let mode = |mode| crate::state::PrefsDelta {
            mode: Some(mode),
            ..Default::default()
        };
        // Each post is followed by its own writer task, as in the loop.
        outbox.post(mode(Mode::Plan));
        let first_writer = outbox.clone();
        outbox.post(mode(Mode::Auto));
        let second_writer = outbox.clone();
        if later_writer_first {
            second_writer.flush(&paths);
            first_writer.flush(&paths);
        } else {
            first_writer.flush(&paths);
            second_writer.flush(&paths);
        }
        assert_eq!(
            pacode_config::load_prefs(&paths).mode,
            Some(Mode::Auto),
            "later writer first: {later_writer_first}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// B12: switching while ↑ shows a history entry brings the real draft back,
/// not the previewed entry.
#[test]
fn a_session_switch_while_browsing_history_keeps_the_real_draft() {
    let mut state = attached_state();
    state.input.history.push_back("old prompt".into());
    state.input.text = "my draft".into();
    state.input.history_up();
    assert_eq!(state.input.text, "old prompt");
    state.leave_active_slot();
    assert_eq!(state.input.text, "my draft");
    assert_eq!(state.input.history_index, None);
}

/// B11: `--analyze-mode` is per process; a session switch must not drop it.
#[test]
fn a_session_switch_keeps_analyze_mode() {
    let mut state = attached_state();
    state.analyze_mode = true;
    state.leave_active_slot();
    state.open_analyze();
    assert!(matches!(
        state.focus,
        Focus::Overlay(crate::state::Overlay::Analyze { .. })
    ));
}

/// B12: the composer belongs to the user — the draft and both histories
/// survive a switch. Prompts queued behind the left session's turn are not
/// sent to the next one; they come back when that session is attached again.
#[test]
fn a_session_switch_keeps_the_composer_and_stashes_the_queue() {
    let mut state = attached_state();
    state.input.text = "half a prompt".into();
    state.input.history.push_back("earlier prompt".into());
    state.input.bash_history.push_back("ls".into());
    assert!(state.input.queue_push("queued".into()));
    state.leave_active_slot();

    assert_eq!(state.input.text, "half a prompt");
    assert_eq!(
        state.input.history.back().map(String::as_str),
        Some("earlier prompt")
    );
    assert_eq!(
        state.input.bash_history.back().map(String::as_str),
        Some("ls")
    );
    assert!(state.input.prompt_queue.is_empty());

    // Another session lands: its queue stays empty.
    let mut other = snapshot(1, PathBuf::from("/tmp"));
    other.meta.id = SessionId::new("ses_other");
    assert!(state.apply_snapshot(other, Instant::now()));
    assert!(state.input.prompt_queue.is_empty());

    // Back to the first one: its queue is handed back.
    state.leave_active_slot();
    assert!(state.apply_snapshot(snapshot(1, PathBuf::from("/tmp")), Instant::now()));
    assert_eq!(
        state.input.prompt_queue.iter().collect::<Vec<_>>(),
        vec!["queued"]
    );
}

/// The panel moved off a subagent (to a task) while its buffer still holds
/// the subagent's items: a cleared context drops that buffer too, owner
/// included, so nothing from before the clear can be painted again.
#[test]
fn context_cleared_drops_the_agent_transcript_whatever_the_panel_shows() {
    let mut state = attached_state();
    let sub = AgentId::new("agt_sub1");
    state.panel.agent_transcript_for(&sub).reset(
        vec![TranscriptItem {
            seq: 1,
            agent: sub.clone(),
            ts_ms: 1,
            kind: TranscriptKind::User {
                text: "subagent work".into(),
            },
        }],
        false,
    );
    let task = PanelTarget::Task(pacode_types::ids::TaskId::new("task"));
    state.panel.target = Some(task.clone());
    assert!(state.panel.owned_agent_transcript(&sub).is_some());

    state.apply_event(2, Event::ContextCleared, Instant::now());

    assert!(state.panel.agent_transcript.cells.is_empty());
    assert!(state.panel.owned_agent_transcript(&sub).is_none());
    assert_eq!(state.panel.target, Some(task), "a task panel stays open");
}
