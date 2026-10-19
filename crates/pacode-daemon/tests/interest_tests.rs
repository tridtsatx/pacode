//! `Subscribe` interest filtering in the session forwarder.

use std::time::Duration;

use pacode_daemon::fanout::run_filtered_forwarder;
use pacode_daemon::interest::{apply, new_shared};
use pacode_types::protocol::{Event, ServerMessage};
use pacode_types::{AgentId, AgentInfo, AgentKind, AgentStatus, Effort, ModelRoute, ToastLevel};
use tokio::sync::{broadcast, mpsc};

fn sub(n: u32) -> AgentId {
    AgentId::new(format!("sub-{n}"))
}

fn info(id: &AgentId, activity: &str) -> AgentInfo {
    AgentInfo {
        id: id.clone(),
        name: id.as_str().to_string(),
        kind: AgentKind::Sub,
        status: AgentStatus::Thinking,
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

fn delta(agent: &AgentId) -> Event {
    Event::TextDelta {
        agent: agent.clone(),
        item_seq: 1,
        text: "x".into(),
    }
}

fn toast() -> Event {
    Event::Toast {
        level: ToastLevel::Info,
        title: "t".into(),
        detail: None,
    }
}

async fn drain(rx: &mut mpsc::Receiver<String>, wait: Duration) -> Vec<(u64, Event)> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(line)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        if let ServerMessage::Event { seq, event } =
            serde_json::from_str::<ServerMessage>(line.trim()).expect("valid")
        {
            out.push((seq, event));
        }
    }
    out
}

#[tokio::test]
async fn unsubscribed_deltas_are_dropped_and_switching_starts_them() {
    let (btx, brx) = broadcast::channel::<(u64, Event)>(1024);
    let (tx, mut rx) = mpsc::channel::<String>(256);
    let interest = new_shared();
    apply(&interest, vec![AgentId::main()], false);
    let handle = tokio::spawn(run_filtered_forwarder(brx, tx, "session", interest.clone()));

    let (a, b) = (sub(1), sub(2));
    let _ = btx.send((1, delta(&a)));
    let _ = btx.send((2, delta(&b)));
    let _ = btx.send((3, delta(&AgentId::main())));
    let _ = btx.send((4, toast()));
    let got = drain(&mut rx, Duration::from_millis(200)).await;
    let seqs: Vec<u64> = got.iter().map(|(s, _)| *s).collect();
    assert_eq!(seqs, vec![3, 4], "only main's delta and the toast pass");

    apply(&interest, vec![AgentId::main(), a.clone()], false);
    let _ = btx.send((5, delta(&a)));
    let _ = btx.send((6, delta(&b)));
    let got = drain(&mut rx, Duration::from_millis(200)).await;
    let seqs: Vec<u64> = got.iter().map(|(s, _)| *s).collect();
    assert_eq!(seqs, vec![5]);
    handle.abort();
}

#[tokio::test]
async fn agent_updates_of_unsubscribed_agents_are_coalesced() {
    let (btx, brx) = broadcast::channel::<(u64, Event)>(1024);
    let (tx, mut rx) = mpsc::channel::<String>(256);
    let interest = new_shared();
    apply(&interest, vec![AgentId::main()], false);
    let handle = tokio::spawn(run_filtered_forwarder(brx, tx, "session", interest));

    let a = sub(1);
    for i in 0..40u64 {
        let _ = btx.send((i + 1, Event::AgentUpdated(info(&a, &format!("step {i}")))));
    }
    let got = drain(&mut rx, Duration::from_millis(1200)).await;
    assert!(!got.is_empty() && got.len() <= 2, "got {}", got.len());
    match &got.last().expect("one flush").1 {
        Event::AgentUpdated(i) => assert_eq!(i.activity.as_deref(), Some("step 39")),
        other => panic!("unexpected {other:?}"),
    }
    handle.abort();
}

#[tokio::test]
async fn default_interest_passes_everything() {
    let (btx, brx) = broadcast::channel::<(u64, Event)>(1024);
    let (tx, mut rx) = mpsc::channel::<String>(256);
    let handle = tokio::spawn(run_filtered_forwarder(brx, tx, "session", new_shared()));
    let a = sub(1);
    let _ = btx.send((1, delta(&a)));
    let _ = btx.send((2, Event::AgentUpdated(info(&a, "x"))));
    let got = drain(&mut rx, Duration::from_millis(200)).await;
    assert_eq!(got.len(), 2);
    handle.abort();
}

#[test]
fn subscribe_and_debug_stats_wire_format() {
    use pacode_types::protocol::{Envelope, Request};
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
