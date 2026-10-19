//! `Subscribe` interest and the forwarder's [`Router`]: what is filtered, what always
//! passes, and the seq contract of coalesced `AgentUpdated` events.

use pacode_types::ids::CallId;
use pacode_types::protocol::{Envelope, Event, Request};
use pacode_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, Effort, ModelRoute, ToastLevel, ToolStatus,
    TranscriptItem, TranscriptKind,
};

use super::{MAX_SUBSCRIBED_AGENTS, Router, apply, new_shared, wants};

fn sub(n: u32) -> AgentId {
    AgentId::new(format!("sub-{n}"))
}

fn info(id: &AgentId, status: AgentStatus, activity: &str) -> AgentInfo {
    AgentInfo {
        id: id.clone(),
        name: id.as_str().to_string(),
        kind: AgentKind::Sub,
        status,
        activity: Some(activity.to_string()),
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

fn update(id: &AgentId, status: AgentStatus, activity: &str) -> Event {
    Event::AgentUpdated(info(id, status, activity))
}

fn delta(agent: &AgentId) -> Event {
    Event::TextDelta {
        agent: agent.clone(),
        item_seq: 1,
        text: "x".into(),
    }
}

fn item(agent: &AgentId, kind: TranscriptKind) -> Event {
    Event::ItemAdded(TranscriptItem {
        seq: 1,
        agent: agent.clone(),
        ts_ms: 0,
        kind,
    })
}

fn tool_call() -> TranscriptKind {
    TranscriptKind::ToolCall {
        call_id: CallId::new("call-1"),
        name: "read".into(),
        title: "Read a.rs".into(),
        intent: None,
        status: ToolStatus::Running,
        preview: String::new(),
        diff: None,
        duration_ms: None,
        task: None,
    }
}

fn toast() -> Event {
    Event::Toast {
        level: ToastLevel::Info,
        title: "t".into(),
        detail: None,
    }
}

/// A router subscribed to main only that has already announced `agents` to the client.
fn narrowed(agents: &[AgentId]) -> (Router, u64) {
    let interest = new_shared();
    apply(&interest, vec![AgentId::main()], false);
    let mut router = Router::new(interest);
    let mut seq = 0;
    for agent in agents {
        seq += 1;
        assert!(
            router
                .route(seq, update(agent, AgentStatus::Thinking, "start"))
                .is_some()
        );
    }
    (router, seq)
}

fn seqs(routed: &[(u64, Event)]) -> Vec<u64> {
    routed.iter().map(|(seq, _)| *seq).collect()
}

#[test]
fn default_interest_wants_everything_and_subscribe_replaces_it() {
    let interest = new_shared();
    assert!(wants(&interest, &sub(1)));
    apply(&interest, vec![AgentId::main()], false);
    assert!(wants(&interest, &AgentId::main()));
    assert!(!wants(&interest, &sub(1)));
    apply(&interest, vec![sub(1)], false);
    assert!(!wants(&interest, &AgentId::main()));
    apply(&interest, Vec::new(), true);
    assert!(wants(&interest, &sub(2)));
}

#[test]
fn explicit_ids_are_capped() {
    let interest = new_shared();
    let agents: Vec<AgentId> = (0..(MAX_SUBSCRIBED_AGENTS as u32 + 10)).map(sub).collect();
    apply(&interest, agents, false);
    let kept = (0..(MAX_SUBSCRIBED_AGENTS as u32 + 10))
        .filter(|n| wants(&interest, &sub(*n)))
        .count();
    assert_eq!(kept, MAX_SUBSCRIBED_AGENTS);
}

#[test]
fn unsubscribed_streams_are_dropped_and_the_rest_passes() {
    let (a, b) = (sub(1), sub(2));
    let (mut router, base) = narrowed(&[a.clone(), b.clone()]);
    let events = [
        delta(&a),
        Event::ReasoningDelta {
            agent: b.clone(),
            item_seq: 1,
            text: "r".into(),
        },
        Event::ToolOutput {
            agent: a.clone(),
            item_seq: 1,
            text: "line".into(),
        },
        item(
            &a,
            TranscriptKind::Assistant {
                text: "hi".into(),
                complete: false,
            },
        ),
        delta(&AgentId::main()),
        toast(),
        item(&b, tool_call()),
    ];
    let mut passed = Vec::new();
    for (offset, event) in events.into_iter().enumerate() {
        if let Some(routed) = router.route(base + 1 + offset as u64, event) {
            passed.push(routed);
        }
    }
    // Main's delta, the session toast and the other agent's tool call (files list).
    assert_eq!(seqs(&passed), vec![base + 5, base + 6, base + 7]);
}

#[test]
fn an_agent_the_client_has_not_seen_is_never_filtered() {
    let (mut router, _) = narrowed(&[]);
    let a = sub(1);
    assert!(router.route(1, delta(&a)).is_some());
    assert!(
        router
            .route(2, update(&a, AgentStatus::Thinking, "x"))
            .is_some()
    );
    assert!(router.route(3, delta(&a)).is_none());
}

#[test]
fn agent_added_announces_the_agent() {
    let (mut router, _) = narrowed(&[]);
    let a = sub(1);
    let added = Event::AgentAdded(info(&a, AgentStatus::Thinking, "spawned"));
    assert!(router.route(1, added).is_some());
    assert!(router.route(2, delta(&a)).is_none());
}

#[test]
fn status_changes_pass_at_once_and_supersede_a_held_update() {
    let a = sub(1);
    let (mut router, base) = narrowed(std::slice::from_ref(&a));
    assert!(
        router
            .route(base + 1, update(&a, AgentStatus::Thinking, "step"))
            .is_none()
    );
    assert!(router.has_held());
    let finished = router
        .route(base + 2, update(&a, AgentStatus::Finished, "done"))
        .expect("a status change is never coalesced");
    assert_eq!(finished.0, base + 2);
    assert!(!router.has_held(), "the newer update replaced the held one");
    // A terminal agent is forgotten, so whatever it still emits passes.
    assert!(router.route(base + 3, delta(&a)).is_some());
}

#[test]
fn coalesced_updates_keep_the_latest_and_reuse_their_own_seq() {
    let a = sub(1);
    let (mut router, base) = narrowed(std::slice::from_ref(&a));
    for step in 1..=40u64 {
        assert!(
            router
                .route(
                    base + step,
                    update(&a, AgentStatus::Thinking, &format!("step {step}"))
                )
                .is_none()
        );
    }
    let flushed = router.flush();
    assert_eq!(seqs(&flushed), vec![base + 40]);
    match &flushed[0].1 {
        Event::AgentUpdated(info) => assert_eq!(info.activity.as_deref(), Some("step 40")),
        other => panic!("unexpected {other:?}"),
    }
    assert!(!router.has_held());
    assert!(router.flush().is_empty());
}

#[test]
fn a_held_update_never_goes_out_below_a_seq_already_sent() {
    let (a, b) = (sub(1), sub(2));
    let (mut router, base) = narrowed(&[a.clone(), b.clone()]);
    // a's update is held at base+1, then main traffic is sent past it.
    assert!(
        router
            .route(base + 1, update(&a, AgentStatus::Thinking, "a"))
            .is_none()
    );
    assert!(router.route(base + 2, delta(&AgentId::main())).is_some());
    assert!(router.route(base + 3, toast()).is_some());
    // No filtered seq above what was sent: nothing can carry it yet.
    assert!(router.flush().is_empty());
    assert!(router.has_held());
    // b's filtered delta at base+4 is the first free seq; a's update rides on it.
    assert!(router.route(base + 4, delta(&b)).is_none());
    let flushed = router.flush();
    assert_eq!(seqs(&flushed), vec![base + 4]);
    assert!(matches!(&flushed[0].1, Event::AgentUpdated(info) if info.id == a));
    // The next event keeps the stream strictly increasing.
    assert_eq!(
        router.route(base + 5, toast()).map(|(seq, _)| seq),
        Some(base + 5)
    );
}

#[test]
fn several_held_updates_get_increasing_carriers() {
    let (a, b, c) = (sub(1), sub(2), sub(3));
    let (mut router, base) = narrowed(&[a.clone(), b.clone(), c.clone()]);
    for (offset, agent) in [&a, &b].into_iter().enumerate() {
        assert!(
            router
                .route(
                    base + 1 + offset as u64,
                    update(agent, AgentStatus::Thinking, "x")
                )
                .is_none()
        );
    }
    assert!(router.route(base + 3, toast()).is_some());
    // One hole: only the older held update goes out, the other waits.
    assert!(router.route(base + 4, delta(&c)).is_none());
    let first = router.flush();
    assert_eq!(seqs(&first), vec![base + 4]);
    assert!(matches!(&first[0].1, Event::AgentUpdated(info) if info.id == a));
    assert!(router.has_held());
    assert!(router.route(base + 5, delta(&c)).is_none());
    let second = router.flush();
    assert_eq!(seqs(&second), vec![base + 5]);
    assert!(matches!(&second[0].1, Event::AgentUpdated(info) if info.id == b));
}

#[test]
fn subscribing_starts_an_agents_stream() {
    let interest = new_shared();
    apply(&interest, vec![AgentId::main()], false);
    let mut router = Router::new(interest.clone());
    let a = sub(1);
    assert!(
        router
            .route(1, update(&a, AgentStatus::Thinking, "x"))
            .is_some()
    );
    assert!(router.route(2, delta(&a)).is_none());
    apply(&interest, vec![AgentId::main(), a.clone()], false);
    assert!(router.route(3, delta(&a)).is_some());
    assert!(
        router
            .route(4, update(&a, AgentStatus::Thinking, "y"))
            .is_some(),
        "a subscribed agent's updates are not coalesced"
    );
}

#[test]
fn reset_forgets_held_updates_after_loss() {
    let a = sub(1);
    let (mut router, base) = narrowed(std::slice::from_ref(&a));
    assert!(
        router
            .route(base + 1, update(&a, AgentStatus::Thinking, "old"))
            .is_none()
    );
    router.reset();
    assert!(!router.has_held());
    assert!(router.flush().is_empty());
    // The agent is unknown again, so its next update passes.
    assert!(
        router
            .route(base + 9, update(&a, AgentStatus::Thinking, "new"))
            .is_some()
    );
}

#[test]
fn subscribe_and_debug_stats_wire_format() {
    let env: Envelope = serde_json::from_str(r#"{"id":3,"type":"subscribe"}"#).expect("defaults");
    assert_eq!(
        env.req,
        Request::Subscribe {
            agents: vec![],
            all: false
        }
    );
    let env: Envelope = serde_json::from_str(r#"{"id":4,"type":"debug_stats"}"#).expect("unit");
    assert_eq!(env.req, Request::DebugStats);
}
