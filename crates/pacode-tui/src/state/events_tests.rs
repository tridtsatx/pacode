use std::time::Instant;

use pacode_client::ClientEvent;
use pacode_types::{AgentId, AgentInfo, AgentKind, AgentStatus, Config, Effort, Event, ModelRoute};

use crate::state::AppState;

fn state() -> AppState {
    let mut s = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    s.dirty = false;
    s
}

fn delta(agent: AgentId) -> ClientEvent {
    ClientEvent::Event {
        seq: 0,
        event: Event::TextDelta {
            agent,
            item_seq: 1,
            text: "hi".into(),
        },
    }
}

fn info(activity: &str) -> AgentInfo {
    AgentInfo {
        id: AgentId::new("sub-1"),
        name: "sub-1".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::Thinking,
        activity: Some(activity.into()),
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: Effort::Medium,
        parent: None,
        summary: None,
        error: None,
    }
}

#[test]
fn delta_of_unfocused_subagent_does_not_dirty() {
    let mut s = state();
    let visible = s.apply_client_event(delta(AgentId::new("sub-1")), Instant::now());
    assert!(!visible);
    assert!(!s.dirty);
}

#[test]
fn main_delta_dirties() {
    let mut s = state();
    assert!(s.apply_client_event(delta(AgentId::main()), Instant::now()));
    assert!(s.dirty);
}

#[test]
fn agent_update_dirties_only_when_the_row_changed() {
    let mut s = state();
    let now = Instant::now();
    let ev = |a: &str| ClientEvent::Event {
        seq: 0,
        event: Event::AgentUpdated(info(a)),
    };
    assert!(s.apply_client_event(ev("reading"), now));
    s.dirty = false;
    assert!(!s.apply_client_event(ev("reading"), now));
    assert!(!s.dirty);
    assert!(s.apply_client_event(ev("writing"), now));
    assert!(s.dirty);
}
