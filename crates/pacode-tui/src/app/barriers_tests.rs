//! The prompt path waits behind a `/clear` and a settings change in flight.

use std::path::PathBuf;
use std::time::Instant;

use pacode_client::ClientEventContext;
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::Mode;
use pacode_types::{
    AgentId, Config, Event, Reply, Request, SessionId, SessionMeta, TurnId, TurnStop,
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

/// A session attached and idle, its prompt gate open.
fn ready() -> AppState {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(SessionMeta {
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
    });
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

fn reply(state: &mut AppState, barrier: RequestBarrier, res: Reply) -> Vec<crate::keys::Action> {
    let scope = ReplyScope::Session {
        context: state.session_context(),
        client_context: client_context(),
    };
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
