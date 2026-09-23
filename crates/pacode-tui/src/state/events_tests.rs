//! `apply_client_event` seq-gap resync tracking and the `TurnEnded` rail
//! fail-safe for a dropped terminal `AgentUpdated`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use pacode_client::ClientEvent;
use pacode_types::ids::{AgentId, SessionId};
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{AgentKind, AgentStatus, Mode};
use pacode_types::{
    AgentInfo, Config, Event, Plan, Request, SessionMeta, SessionSnapshot, SessionStatusKind,
    TurnId, TurnStop, UsageTotals,
};

use crate::state::{AppState, Connection, RESYNC_DEBOUNCE_MS, SessionSlot};

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
    state
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
        cron_jobs: vec![],
        monitors: vec![],
        seq,
    }
}

fn ev(seq: u64) -> ClientEvent {
    ClientEvent::Event {
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
        seq: 1,
        event: Event::TurnEnded {
            agent: id.clone(),
            turn: TurnId::new("turn_1"),
            usage: None,
            stop,
        },
    }
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
    state.apply_client_event(ClientEvent::Snapshot(snapshot(7, cwd)), now);
    assert!(!state.resync_in_flight);
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, Some(7));

    // Contiguous seqs after the snapshot do not re-arm.
    state.apply_client_event(ev(8), now);
    assert!(!state.resync_want);
    assert!(state.take_resync_request(now).is_none());
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
    assert_eq!(state.last_event_seq, Some(9));
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
            seq: 1,
            event: Event::PermissionRequested(req.clone()),
        },
        now,
    );
    // The daemon records the same request as a transcript item right after —
    // it must refresh the prompt, not render a second one.
    state.apply_client_event(
        ClientEvent::Event {
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

    state.apply_client_event(ClientEvent::Snapshot(snap), now);

    assert_eq!(permission_cell_count(&state), 1);
}

#[test]
fn permission_explained_folds_into_the_pending_prompt() {
    let mut state = attached_state();
    let now = Instant::now();
    state.apply_client_event(
        ClientEvent::Event {
            seq: 1,
            event: Event::PermissionRequested(perm_request("perm_1")),
        },
        now,
    );
    state.apply_client_event(
        ClientEvent::Event {
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
            seq: 1,
            event: Event::PermissionRequested(perm_request("perm_1")),
        },
        now,
    );
    state.apply_client_event(
        ClientEvent::Event {
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
    // rather than vanishing.
    state.apply_client_event(
        ClientEvent::Event {
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
}

#[test]
fn permission_explained_without_level_keeps_the_honest_text() {
    let mut state = attached_state();
    let now = Instant::now();
    state.apply_client_event(
        ClientEvent::Event {
            seq: 1,
            event: Event::PermissionRequested(perm_request("perm_1")),
        },
        now,
    );
    state.apply_client_event(
        ClientEvent::Event {
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
            seq: 8,
            event: Event::PlanUpdated(Plan::default()),
        },
        now,
    );
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, Some(8));
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
}

#[test]
fn attach_snapshot_reseeds_the_slot_status() {
    let mut state = attached_state();
    let now = Instant::now();
    state.slots[0] = Some(slot("ses_test", SessionStatusKind::Done));

    // A quiet attach clears `Done` — the session is being viewed.
    state.apply_client_event(
        ClientEvent::Snapshot(snapshot(5, PathBuf::from("/tmp"))),
        now,
    );
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::Idle
    );

    // A snapshot with a running turn seeds Working.
    let mut snap = snapshot(6, PathBuf::from("/tmp"));
    snap.turn_active = true;
    state.apply_client_event(ClientEvent::Snapshot(snap), now);
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::Working
    );

    // A snapshot with a pending permission seeds NeedsInput.
    let mut snap = snapshot(7, PathBuf::from("/tmp"));
    snap.pending_permissions = vec![perm_request("perm_9")];
    state.apply_client_event(ClientEvent::Snapshot(snap), now);
    assert_eq!(
        state.slots[0].as_ref().unwrap().status,
        SessionStatusKind::NeedsInput
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
