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

fn ask(sub: &Agent, parent: &Agent, call: &str) -> oneshot::Receiver<Reply> {
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
    assert!(matches!(
        sub.mailbox.offer(&sub, "hi", Consent::Absent),
        Offer::Queued
    ));
    assert_eq!(queued_messages(&sub), 1);
}

#[test]
fn an_idle_sealed_or_stopping_agent_is_resumed_instead() {
    let sub = agent("sub");
    assert!(matches!(
        sub.mailbox.offer(&sub, "hi", Consent::Absent),
        Offer::Ended
    ));

    let turn = sub.turn_lock.try_lock().unwrap();
    assert!(sub.mailbox.seal_unless_message(&sub.injections));
    assert!(matches!(
        sub.mailbox.offer(&sub, "hi", Consent::Absent),
        Offer::Ended
    ));
    sub.mailbox.unseal();
    sub.mailbox.note_stop(&sub, StopOrigin::Orchestrator);
    assert!(matches!(
        sub.mailbox.offer(&sub, "hi", Consent::Absent),
        Offer::Ended
    ));
    drop(turn);
    assert_eq!(queued_messages(&sub), 0);
}

#[test]
fn a_message_that_beat_the_seal_keeps_the_turn_alive() {
    let sub = agent("sub");
    let _turn = sub.turn_lock.try_lock().unwrap();
    assert!(matches!(
        sub.mailbox.offer(&sub, "late", Consent::Absent),
        Offer::Queued
    ));
    assert!(
        !sub.mailbox.seal_unless_message(&sub.injections),
        "a queued message must re-drive the turn, not be sealed away"
    );
    // Still unsealed: the next message rides the same re-drive.
    assert!(matches!(
        sub.mailbox.offer(&sub, "later", Consent::Absent),
        Offer::Queued
    ));
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

    let Offer::Answered {
        call_id: call,
        taken,
    } = sub.mailbox.offer(&sub, "postgres", Consent::Absent)
    else {
        panic!("the open question takes the message");
    };
    assert_eq!(call, CallId::new("c1"));
    let reply = rx.await.unwrap();
    assert_eq!(reply.reply, ParentReply::Answered("postgres".into()));
    // The asking call confirms it took the answer.
    reply.taken.unwrap().send(()).unwrap();
    assert!(taken.await.is_ok());
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
    assert!(matches!(
        sub.mailbox.offer(&sub, "hi", Consent::Absent),
        Offer::Queued
    ));
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
    assert_eq!(rx.await.unwrap().reply, ParentReply::ParentFailed);
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
    sub.mailbox.note_stop(&sub, StopOrigin::Orchestrator);
    assert!(matches!(
        sub.mailbox.offer(&sub, "late", Consent::Absent),
        Offer::Ended
    ));
    assert!(
        rx.try_recv().is_err(),
        "no answer to a question being cancelled"
    );

    sub.mailbox.note_stop(&sub, StopOrigin::User);
    assert!(matches!(
        sub.mailbox.offer(&sub, "late", Consent::Absent),
        Offer::StoppedByUser
    ));
    sub.mailbox.note_stop(&sub, StopOrigin::Orchestrator);
    assert_eq!(
        sub.mailbox.stopped_by(),
        Some(StopOrigin::User),
        "a user stop is never downgraded"
    );
}

/// An answer the asking call never took — a stop dropped the call first —
/// is not an answer: the `send` that handed it over learns so.
#[tokio::test]
async fn an_answer_the_call_never_took_fails_its_send() {
    let parent = agent("main");
    let sub = agent("sub");
    let _turn = sub.turn_lock.try_lock().unwrap();
    let rx = ask(&sub, &parent, "c1");
    let Offer::Answered { taken, .. } = sub.mailbox.offer(&sub, "postgres", Consent::Absent) else {
        panic!("the open question takes the message");
    };
    drop(rx);
    assert!(taken.await.is_err());
}

/// A resume lifts the stop only if no stop and no close landed after it read
/// the generation; one that did stands, origin and request included.
#[test]
fn a_resume_yields_to_a_stop_that_landed_after_it_looked() {
    let sub = agent("sub");
    sub.mailbox.note_stop(&sub, StopOrigin::Orchestrator);
    sub.request_stop();

    let generation = sub.turn_generation();
    sub.mailbox.note_stop(&sub, StopOrigin::User);
    assert!(matches!(
        sub.mailbox.lift_stop(&sub, generation),
        Lift::Superseded
    ));
    assert_eq!(sub.mailbox.stopped_by(), Some(StopOrigin::User));
    assert!(sub.stop_requested());

    // A close stops the agent without the mailbox lock.
    let generation = sub.turn_generation();
    sub.request_stop();
    assert!(matches!(
        sub.mailbox.lift_stop(&sub, generation),
        Lift::Superseded
    ));
    assert!(sub.stop_requested());

    let generation = sub.turn_generation();
    assert!(matches!(
        sub.mailbox.lift_stop(&sub, generation),
        Lift::Lifted {
            was: Some(StopOrigin::User)
        }
    ));
    assert_eq!(sub.mailbox.stopped_by(), None);
    assert!(!sub.stop_requested());
}

#[tokio::test]
async fn questions_stop_being_sent_after_too_many_go_unanswered() {
    let parent = agent("main");
    let sub = agent("sub");
    for n in 0..MAX_UNANSWERED_IN_ROW {
        let rx = ask(&sub, &parent, &format!("c{n}"));
        sub.mailbox.mark_delivered(&CallId::new(format!("c{n}")));
        sub.mailbox.resolve_delivered(ParentReply::Unanswered);
        assert_eq!(rx.await.unwrap().reply, ParentReply::Unanswered);
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
        sub.mailbox.offer(&sub, "still here", Consent::Absent),
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

/// A turn that failed is not an orchestrator that chose not to answer: a run
/// of provider or store failures never stops the questions from being sent.
#[tokio::test]
async fn failed_parent_turns_do_not_count_as_unanswered() {
    let parent = agent("main");
    let sub = agent("sub");
    for n in 0..MAX_UNANSWERED_IN_ROW {
        let rx = ask(&sub, &parent, &format!("c{n}"));
        sub.mailbox.mark_delivered(&CallId::new(format!("c{n}")));
        sub.mailbox.resolve_delivered(ParentReply::ParentFailed);
        assert_eq!(rx.await.unwrap().reply, ParentReply::ParentFailed);
    }
    assert!(matches!(
        sub.mailbox.open(&parent, question("next")).unwrap(),
        Opened::Asked(_)
    ));
}

/// The user's leave to lift their stop names that stop: it does not cover a
/// stop they made after they were asked.
#[test]
fn leave_to_lift_a_user_stop_names_that_stop() {
    let sub = agent("sub");
    assert_eq!(sub.mailbox.user_stop(), None);
    sub.mailbox.note_stop(&sub, StopOrigin::User);
    let stop = sub.mailbox.user_stop().unwrap();
    assert!(sub.mailbox.held_by_user(Consent::Absent));
    assert!(!sub.mailbox.held_by_user(Consent::Given(stop)));
    assert!(!sub.mailbox.held_by_user(Consent::Own));
    assert!(matches!(
        sub.mailbox.offer(&sub, "hi", Consent::Given(stop)),
        Offer::Ended
    ));

    sub.mailbox.note_stop(&sub, StopOrigin::User);
    assert!(sub.mailbox.held_by_user(Consent::Given(stop)));
    assert!(matches!(
        sub.mailbox.offer(&sub, "hi", Consent::Given(stop)),
        Offer::StoppedByUser
    ));
}
