//! The mailbox decisions in isolation: what `send` does in each state, the
//! seal that keeps a message from being dropped with a finishing turn, stops,
//! the unanswered cap and the hand-over on detach.

use std::sync::Arc;

use pacode_tools::ToolRegistry;
use pacode_types::{AgentId, AgentInfo, AgentKind, AgentStatus, CallId, ModelRoute};

use super::*;
use crate::agent::History;

fn agent(id: &str) -> Arc<Agent> {
    let id = AgentId::new(id);
    let info = AgentInfo {
        id: id.clone(),
        name: id.to_string(),
        kind: AgentKind::Sub,
        status: AgentStatus::Thinking,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: pacode_types::Effort::Low,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    Arc::new(Agent::new(
        id,
        info,
        History::default(),
        ToolRegistry::new(),
        50,
        None,
    ))
}

fn question(call: &str) -> Injection {
    Injection::AgentQuestion {
        agent: AgentId::new("sub"),
        name: "sub".into(),
        call_id: CallId::new(call),
        question: "which?".into(),
    }
}

fn ask(sub: &Agent, parent: &Agent, call: &str) -> oneshot::Receiver<ParentReply> {
    match sub.mailbox.open(parent, question(call)).unwrap() {
        Opened::Asked(rx) => rx,
        Opened::NotAnswering => panic!("the question was not sent"),
    }
}

fn queued_messages(agent: &Agent) -> usize {
    agent
        .injections
        .drain()
        .iter()
        .filter(|i| matches!(i, Injection::ParentMessage { .. }))
        .count()
}

#[test]
fn a_running_turn_queues_the_message() {
    let sub = agent("sub");
    let _turn = sub.turn_lock.try_lock().unwrap();
    assert!(matches!(sub.mailbox.offer(&sub, "hi"), Offer::Queued));
    assert_eq!(queued_messages(&sub), 1);
}

#[test]
fn an_idle_sealed_or_stopping_agent_is_resumed_instead() {
    let sub = agent("sub");
    assert!(matches!(sub.mailbox.offer(&sub, "hi"), Offer::Ended));

    let turn = sub.turn_lock.try_lock().unwrap();
    assert!(sub.mailbox.seal_unless_message(&sub.injections));
    assert!(matches!(sub.mailbox.offer(&sub, "hi"), Offer::Ended));
    sub.mailbox.unseal();
    sub.mailbox.note_stop(StopOrigin::Orchestrator);
    assert!(matches!(sub.mailbox.offer(&sub, "hi"), Offer::Ended));
    drop(turn);
    assert_eq!(queued_messages(&sub), 0);
}

#[test]
fn a_message_that_beat_the_seal_keeps_the_turn_alive() {
    let sub = agent("sub");
    let _turn = sub.turn_lock.try_lock().unwrap();
    assert!(matches!(sub.mailbox.offer(&sub, "late"), Offer::Queued));
    assert!(
        !sub.mailbox.seal_unless_message(&sub.injections),
        "a queued message must re-drive the turn, not be sealed away"
    );
    // Still unsealed: the next message rides the same re-drive.
    assert!(matches!(sub.mailbox.offer(&sub, "later"), Offer::Queued));
}

#[tokio::test]
async fn an_open_question_takes_the_message_as_its_answer() {
    let parent = agent("main");
    let sub = agent("sub");
    let _turn = sub.turn_lock.try_lock().unwrap();
    let rx = ask(&sub, &parent, "c1");
    assert!(
        !parent.injections.is_empty(),
        "the question is queued on the parent"
    );
    assert!(
        sub.mailbox.open(&parent, question("c2")).is_err(),
        "one open question per subagent"
    );

    let Offer::Answered(call) = sub.mailbox.offer(&sub, "postgres") else {
        panic!("the open question takes the message");
    };
    assert_eq!(call, CallId::new("c1"));
    assert_eq!(rx.await.unwrap(), ParentReply::Answered("postgres".into()));
    parent
        .injections
        .retract_question(&AgentId::new("sub"), &call);
    assert!(parent.injections.is_empty());
    assert_eq!(queued_messages(&sub), 0, "an answer is not also a message");
}

#[test]
fn a_question_whose_call_is_gone_does_not_swallow_the_message() {
    let parent = agent("main");
    let sub = agent("sub");
    let _turn = sub.turn_lock.try_lock().unwrap();
    drop(ask(&sub, &parent, "c1"));
    assert!(matches!(sub.mailbox.offer(&sub, "hi"), Offer::Queued));
}

#[tokio::test]
async fn only_a_delivered_question_ends_with_the_turn() {
    let parent = agent("main");
    let sub = agent("sub");
    let rx = ask(&sub, &parent, "c1");
    sub.mailbox.resolve_delivered(ParentReply::Unanswered);
    sub.mailbox.mark_delivered(&CallId::new("other"));
    sub.mailbox.resolve_delivered(ParentReply::Unanswered);
    sub.mailbox.mark_delivered(&CallId::new("c1"));
    sub.mailbox.resolve_delivered(ParentReply::ParentFailed);
    assert_eq!(rx.await.unwrap(), ParentReply::ParentFailed);
    assert!(!sub.mailbox.take_question(&CallId::new("c1")));
}

/// A stop and an answer serialise on the mailbox: once the stop is noted the
/// question is never answered (its call is about to be dropped), and a user
/// stop is reported as such.
#[tokio::test]
async fn a_stopping_agent_takes_no_answer() {
    let parent = agent("main");
    let sub = agent("sub");
    let _turn = sub.turn_lock.try_lock().unwrap();
    let mut rx = ask(&sub, &parent, "c1");
    sub.mailbox.note_stop(StopOrigin::Orchestrator);
    assert!(matches!(sub.mailbox.offer(&sub, "late"), Offer::Ended));
    assert!(
        rx.try_recv().is_err(),
        "no answer to a question being cancelled"
    );

    sub.mailbox.note_stop(StopOrigin::User);
    assert!(matches!(
        sub.mailbox.offer(&sub, "late"),
        Offer::StoppedByUser
    ));
    sub.mailbox.note_stop(StopOrigin::Orchestrator);
    assert_eq!(
        sub.mailbox.stopped_by(),
        Some(StopOrigin::User),
        "a user stop is never downgraded"
    );
    sub.mailbox.clear_stop();
    assert_eq!(sub.mailbox.stopped_by(), None);
}

#[tokio::test]
async fn questions_stop_being_sent_after_too_many_go_unanswered() {
    let parent = agent("main");
    let sub = agent("sub");
    for n in 0..MAX_UNANSWERED_IN_ROW {
        let rx = ask(&sub, &parent, &format!("c{n}"));
        sub.mailbox.mark_delivered(&CallId::new(format!("c{n}")));
        sub.mailbox.resolve_delivered(ParentReply::Unanswered);
        assert_eq!(rx.await.unwrap(), ParentReply::Unanswered);
    }
    parent.injections.drain();
    assert!(matches!(
        sub.mailbox.open(&parent, question("next")).unwrap(),
        Opened::NotAnswering
    ));
    assert!(
        parent.injections.is_empty(),
        "the parent is not woken for it"
    );

    // Any message from the orchestrator counts as engaging again.
    let _turn = sub.turn_lock.try_lock().unwrap();
    assert!(matches!(
        sub.mailbox.offer(&sub, "still here"),
        Offer::Queued
    ));
    drop(ask(&sub, &parent, "after"));
}

#[test]
fn a_detach_hands_the_open_question_over_undelivered() {
    let parent = agent("main");
    let sub = agent("sub");
    let _rx = ask(&sub, &parent, "c1");
    sub.mailbox.mark_delivered(&CallId::new("c1"));
    assert_eq!(sub.mailbox.undeliver(), Some(question("c1")));
    // Undelivered again: the old turn's end no longer settles it.
    sub.mailbox.resolve_delivered(ParentReply::Unanswered);
    assert!(sub.mailbox.take_question(&CallId::new("c1")));
}
