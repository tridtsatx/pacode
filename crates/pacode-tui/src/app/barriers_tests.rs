//! The prompt path waits behind a `/clear` and a settings change in flight.

use std::path::PathBuf;
use std::time::Instant;

use pacode_client::ClientEventContext;
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{Mode, Plan, UsageTotals};
use pacode_types::{
    AgentId, Config, Event, Reply, Request, SessionId, SessionMeta, SessionSnapshot,
    TranscriptItem, TranscriptKind, TurnId, TurnStop,
};

use super::super::bg_response::{BgResponse, handle_bg_response};
use super::super::reply::ReplyScope;
use super::prompt_gate_open;
use crate::state::{AppState, RequestBarrier};

fn client_context() -> ClientEventContext {
    ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    }
}

fn meta() -> SessionMeta {
    SessionMeta {
        id: SessionId::new("ses_test"),
        name: None,
        cwd: PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        model: ModelRoute::new("prov", "model"),
        effort: Effort::Medium,
        mode: Mode::Build,
        first_prompt: None,
        zdr: false,
    }
}

/// The resync snapshot: `transcript` empty or holding one user message.
fn snapshot(seq: u64, transcript: Option<&str>) -> SessionSnapshot {
    SessionSnapshot {
        meta: meta(),
        agents: Vec::new(),
        plan: Plan::default(),
        tasks: Vec::new(),
        usage: UsageTotals::default(),
        transcript: transcript
            .map(|text| TranscriptItem {
                seq: 1,
                agent: AgentId::main(),
                ts_ms: 1,
                kind: TranscriptKind::User { text: text.into() },
            })
            .into_iter()
            .collect(),
        has_more_history: false,
        pending_permissions: Vec::new(),
        pending_questions: Vec::new(),
        turn_active: false,
        active_turn: None,
        terminal_turn: None,
        terminal_error: None,
        cron_jobs: Vec::new(),
        monitors: Vec::new(),
        seq,
    }
}

/// A session attached and idle, its prompt gate open.
fn ready() -> AppState {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta());
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    assert!(prompt_gate_open(&state));
    state
}

fn event(state: &mut AppState, seq: u64, event: Event) {
    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: client_context(),
            seq,
            event,
        },
        Instant::now(),
    );
}

fn scope(state: &AppState) -> ReplyScope {
    ReplyScope::Session {
        context: state.session_context(),
        client_context: client_context(),
    }
}

fn reply(state: &mut AppState, barrier: RequestBarrier, res: Reply) -> Vec<crate::keys::Action> {
    let scope = scope(state);
    reply_in(state, scope, barrier, res)
}

fn reply_in(
    state: &mut AppState,
    scope: ReplyScope,
    barrier: RequestBarrier,
    res: Reply,
) -> Vec<crate::keys::Action> {
    handle_bg_response(
        BgResponse::BarrierReply {
            barrier,
            scope,
            res: Ok(res),
        },
        state,
    )
}

fn user(text: &str) -> Request {
    Request::UserMessage {
        text: text.into(),
        images: Vec::new(),
        message_id: None,
    }
}

/// `/clear` during a turn: the interrupt ends the turn, which used to drain
/// the next queued prompt before `ContextCleared` — into the sweep, or into
/// the cleared context. It now waits, and the clear drops it.
#[test]
fn a_clear_in_flight_holds_the_queue_and_the_clear_drops_it() {
    let mut state = ready();
    let turn = TurnId::new("turn_1");
    event(
        &mut state,
        1,
        Event::TurnStarted {
            agent: AgentId::main(),
            turn: turn.clone(),
        },
    );
    assert!(state.turn_active);
    state
        .input
        .queue_push("typed for the old conversation".into());
    state.input.queue_push("and another".into());

    let barrier = RequestBarrier::for_request(&Request::ClearContext);
    assert_eq!(barrier, Some(RequestBarrier::Clear));
    state.raise_barrier(RequestBarrier::Clear);
    event(
        &mut state,
        2,
        Event::TurnEnded {
            agent: AgentId::main(),
            turn,
            usage: None,
            stop: TurnStop::Interrupted,
        },
    );
    assert!(!state.turn_active);
    assert!(!prompt_gate_open(&state));
    assert_eq!(state.drain_prompt_queue(), None, "held behind the clear");
    assert_eq!(state.input.prompt_queue.len(), 2);

    event(&mut state, 3, Event::ContextCleared);
    assert!(state.input.prompt_queue.is_empty());
    assert!(!state.clear_in_flight);
    assert!(
        state
            .toasts
            .iter()
            .any(|toast| toast.title == "2 queued prompts dropped")
    );
    // The reply after the event finds nothing left to drop.
    reply(&mut state, RequestBarrier::Clear, Reply::Ok);
    assert!(prompt_gate_open(&state));
    assert_eq!(state.toasts.len(), 1);
}

/// The reply can beat `ContextCleared` to the client: an `Ok` clear drops the
/// queue itself rather than letting it drain ahead of the event.
#[test]
fn a_clear_answered_before_its_event_still_drops_the_queue() {
    let mut state = ready();
    state.raise_barrier(RequestBarrier::Clear);
    state.input.queue_push("old".into());
    reply(&mut state, RequestBarrier::Clear, Reply::Ok);
    assert_eq!(state.drain_prompt_queue(), None);
    assert!(state.input.prompt_queue.is_empty());
    assert!(
        state
            .toasts
            .iter()
            .any(|toast| toast.title == "1 queued prompt dropped")
    );
}

/// A refused clear leaves the conversation, so the queue drains as before.
#[test]
fn a_refused_clear_releases_the_queue() {
    let mut state = ready();
    state.raise_barrier(RequestBarrier::Clear);
    state.input.queue_push("still wanted".into());
    reply(
        &mut state,
        RequestBarrier::Clear,
        Reply::Error {
            message: "refused".into(),
        },
    );
    assert_eq!(state.drain_prompt_queue(), Some(user("still wanted")));
}

/// A prompt sent right after a model switch waits for the switch's reply, so
/// it cannot reach the daemon first and run on the old route; then it drains.
#[test]
fn a_prompt_waits_behind_a_settings_change_in_flight() {
    let mut state = ready();
    let barrier = RequestBarrier::for_request(&Request::SetModel(ModelRoute::new("p", "m")));
    assert_eq!(barrier, Some(RequestBarrier::Settings));
    state.raise_barrier(RequestBarrier::Settings);
    assert!(!prompt_gate_open(&state), "a send queues instead");
    state.input.queue_push("after the switch".into());
    assert_eq!(state.drain_prompt_queue(), None);

    reply(&mut state, RequestBarrier::Settings, Reply::Ok);
    assert!(prompt_gate_open(&state));
    assert_eq!(state.drain_prompt_queue(), Some(user("after the switch")));
}

/// A settings run queued without a session drains ahead of the prompts
/// queued after it, and those prompts wait for its reply instead of draining
/// on the next event.
#[test]
fn a_queued_settings_run_drains_before_the_prompts_and_holds_them() {
    let mut state = ready();
    let run = vec![
        Request::SetModel(ModelRoute::new("p", "m")),
        Request::SetEffort(Effort::Low),
    ];
    assert!(state.session.queue_sequence(crate::state::PromptBatch {
        requests: run.clone(),
    }));
    state.input.queue_push("after the run".into());

    assert_eq!(state.drain_prompt_queue(), None, "the run goes first");
    let drained = state.drain_pending_sequence().map(|batch| batch.requests);
    assert_eq!(drained, Some(run));
    // `send_ordered` raises the barrier as it sends the run.
    state.raise_barrier(RequestBarrier::Settings);
    assert_eq!(state.drain_prompt_queue(), None, "held until the reply");

    reply(&mut state, RequestBarrier::Settings, Reply::Ok);
    assert_eq!(state.drain_prompt_queue(), Some(user("after the run")));
}

/// A connection loss writes the barriers off: their replies will never fold.
#[test]
fn a_disconnect_drops_the_barriers() {
    let mut state = ready();
    state.raise_barrier(RequestBarrier::Clear);
    state.raise_barrier(RequestBarrier::Settings);
    state.apply_client_event(
        pacode_client::ClientEvent::Disconnected {
            context: client_context(),
            reason: "gone".into(),
        },
        Instant::now(),
    );
    assert!(!state.prompt_path_held());
}

/// Resync while a settings change is in flight: the resync writes the barrier
/// off (its reply is scoped to the old epoch and can never fold), the queue
/// drains once the snapshot lands, and the stale reply lowers nothing newer.
#[test]
fn a_resync_releases_a_settings_barrier_and_its_stale_reply_lowers_nothing() {
    let mut state = ready();
    state.raise_barrier(RequestBarrier::Settings);
    let stale = scope(&state);
    state.input.queue_push("after the switch".into());

    state.resync_want = true;
    assert_eq!(
        state.take_resync_request(Instant::now()),
        Some(Request::GetSnapshot)
    );
    assert!(!state.prompt_path_held());
    assert!(state.apply_snapshot(snapshot(5, Some("before")), Instant::now()));
    assert_eq!(state.drain_prompt_queue(), Some(user("after the switch")));

    state.raise_barrier(RequestBarrier::Settings);
    reply_in(&mut state, stale, RequestBarrier::Settings, Reply::Ok);
    assert_eq!(state.settings_in_flight, 1, "the newer barrier stays");
}

/// A clear whose reply the resync made stale: the snapshot decides. An empty
/// transcript means the clear went through, so the queue goes with a toast.
#[test]
fn a_clear_lost_to_a_resync_drops_the_queue_when_the_snapshot_is_empty() {
    let mut state = ready();
    state.raise_barrier(RequestBarrier::Clear);
    let stale = scope(&state);
    state.input.queue_push("old".into());
    state.resync_want = true;
    assert!(state.take_resync_request(Instant::now()).is_some());
    assert!(!state.prompt_path_held());

    assert!(state.apply_snapshot(snapshot(5, None), Instant::now()));
    assert!(state.input.prompt_queue.is_empty());
    assert!(
        state
            .toasts
            .iter()
            .any(|toast| toast.title == "1 queued prompt dropped")
    );
    reply_in(&mut state, stale, RequestBarrier::Clear, Reply::Ok);
    assert_eq!(state.toasts.len(), 1);
}

/// The same, but the snapshot still holds the conversation: the prompts stay
/// and drain.
#[test]
fn a_clear_lost_to_a_resync_keeps_the_queue_when_the_snapshot_is_not_empty() {
    let mut state = ready();
    state.raise_barrier(RequestBarrier::Clear);
    state.input.queue_push("still wanted".into());
    state.resync_want = true;
    assert!(state.take_resync_request(Instant::now()).is_some());

    assert!(state.apply_snapshot(snapshot(5, Some("kept")), Instant::now()));
    assert!(!state.prompt_path_held());
    assert_eq!(state.drain_prompt_queue(), Some(user("still wanted")));
    assert!(state.toasts.is_empty());
}

/// `ContextCleared` already dropped the old queue; a prompt queued after it is
/// for the new conversation, and the clear's late `Ok` must not drop it.
#[test]
fn a_late_clear_reply_spares_prompts_queued_after_the_event() {
    let mut state = ready();
    state.raise_barrier(RequestBarrier::Clear);
    event(&mut state, 1, Event::ContextCleared);
    state.input.queue_push("for the new conversation".into());
    reply(&mut state, RequestBarrier::Clear, Reply::Ok);
    assert_eq!(
        state.drain_prompt_queue(),
        Some(user("for the new conversation"))
    );
}
