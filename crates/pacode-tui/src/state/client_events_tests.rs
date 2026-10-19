//! Seq gaps on a stream the daemon filters by `Subscribe`: holes are expected once a
//! subagent is known, and loss still arrives as an explicit marker.

use std::path::PathBuf;
use std::time::Instant;

use pacode_client::{ClientEvent, ClientEventContext};
use pacode_types::ids::{AgentId, SessionId};
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{AgentKind, AgentStatus, Mode};
use pacode_types::{AgentInfo, Config, Event, Plan, Request, SessionMeta};

use crate::state::{AppState, Connection};

fn attached_state() -> AppState {
    let mut state = AppState::new(Config::default(), "1.0.0".to_string(), 120, 34);
    state.meta = Some(SessionMeta {
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
    });
    state.connection = Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state
}

fn context() -> ClientEventContext {
    ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    }
}

fn event(seq: u64, event: Event) -> ClientEvent {
    ClientEvent::Event {
        context: context(),
        seq,
        event,
    }
}

fn plan(seq: u64) -> ClientEvent {
    event(seq, Event::PlanUpdated(Plan::default()))
}

fn sub_agent() -> AgentInfo {
    AgentInfo {
        id: AgentId::new("sub-1"),
        name: "sub".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::Thinking,
        activity: None,
        started_at_ms: 1,
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

#[test]
fn a_gap_without_subagents_is_loss() {
    let mut state = attached_state();
    let now = Instant::now();
    state.apply_client_event(plan(1), now);
    state.apply_client_event(plan(3), now);
    assert!(state.resync_want);
}

#[test]
fn a_gap_after_a_subagent_appeared_is_the_interest_filter() {
    let mut state = attached_state();
    let now = Instant::now();
    state.apply_client_event(plan(1), now);
    state.apply_client_event(event(2, Event::AgentUpdated(sub_agent())), now);
    // The daemon dropped the subagent's deltas 3..=9.
    assert!(state.apply_client_event(plan(10), now));
    assert!(!state.resync_want);
    assert_eq!(state.last_event_seq, Some(10));
    // An older seq is still stale, never applied.
    assert!(!state.apply_client_event(plan(7), now));
}

#[test]
fn loss_on_a_filtered_stream_still_resyncs() {
    let mut state = attached_state();
    let now = Instant::now();
    state.apply_client_event(event(1, Event::AgentUpdated(sub_agent())), now);
    assert!(state.apply_client_event(ClientEvent::ResyncNeeded { context: context() }, now,));
    assert!(state.resync_want);
    assert_eq!(state.take_resync_request(now), Some(Request::GetSnapshot));
}
