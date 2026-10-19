//! An agent the user stopped, when the orchestrator wants it back: the
//! `agent` tool asks the user, and only a "Resume" for that very stop lifts
//! it.

use std::sync::Arc;

use pacode_types::{AgentId, AgentStatus, Question, QuestionAnswer};
use serde_json::json;
use tokio::sync::Semaphore;

use super::StopOrigin;
use super::harness::*;
use crate::agent::Agent;

/// One subagent, stopped by the user, with the orchestrator idle.
async fn user_stopped(h: &Harness) -> (Arc<Semaphore>, Arc<Agent>) {
    let gate = spawn_parked(h, text("partial")).await;
    let sub = h.sub();
    h.session.stop_agent(&sub.id()).await.unwrap();
    wait_for("the stop", || !sub.is_running()).await;
    (gate, sub)
}

/// The orchestrator calls `agent <action>` on `id` (the message is ignored by
/// `continue`), then ends its turn on whatever it was told.
async fn orchestrator_calls(h: &Harness, action: &str, id: &AgentId) {
    h.provider.script(
        false,
        vec![
            tool(
                "agent",
                json!({"action": action, "agent_id": id.as_str(), "message": "go on"}),
            ),
            text("done"),
        ],
    );
    h.session
        .submit_user_message("bring it back".into())
        .await
        .unwrap();
}

async fn asked(h: &Harness) -> Question {
    wait_for("the user to be asked", || {
        !h.session.questions.pending().is_empty()
    })
    .await;
    h.session.questions.pending().remove(0)
}

/// What the orchestrator was told about its call, once its turn ended.
async fn told(h: &Harness) -> String {
    wait_for("the orchestrator's turn to end", || {
        h.main_turns_ended() >= 2 && h.main_idle()
    })
    .await;
    h.provider.seen(false).last().unwrap().clone()
}

async fn stays_stopped(h: &Harness, sub: &Agent) {
    assert_eq!(sub.status(), AgentStatus::Stopped);
    assert!(!sub.is_running());
    assert_eq!(sub.mailbox.stopped_by(), Some(StopOrigin::User));
    assert_eq!(h.provider.seen(true).len(), 1, "no turn was started");
    let stored = h
        .session
        .store
        .agent_restore(&h.session.id, &sub.id())
        .await
        .unwrap();
    assert_eq!(stored.stopped_by.as_deref(), Some("user"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_user_agreeing_resumes_an_agent_they_stopped() {
    for action in ["send", "continue"] {
        let h = Harness::new().await;
        let (gate, sub) = user_stopped(&h).await;
        h.provider.script(true, vec![text("resumed work")]);
        orchestrator_calls(&h, action, &sub.id()).await;

        let question = asked(&h).await;
        assert!(question.question.contains("You stopped it"), "{question:?}");
        assert_eq!(question.question.contains("go on"), action == "send");
        h.session
            .questions
            .resolve(&question.id, QuestionAnswer::choice(0));

        wait_for("the resumed turn", || h.provider.seen(true).len() == 2).await;
        let resumed = h.provider.seen(true)[1].clone();
        let input = match action {
            "send" => "<orchestrator_message>\ngo on",
            _ => "[resumed by orchestrator",
        };
        assert!(resumed.contains(input), "{action}: {resumed}");
        assert!(told(&h).await.contains("resumed"), "{action}");
        assert_eq!(sub.mailbox.stopped_by(), None, "{action}");
        let stored = h
            .session
            .store
            .agent_restore(&h.session.id, &sub.id())
            .await
            .unwrap();
        assert_eq!(stored.stopped_by, None, "{action}");
        gate.add_permits(1);
        h.core.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_user_keeping_it_stopped_is_final() {
    for action in ["send", "continue"] {
        let h = Harness::new().await;
        let (gate, sub) = user_stopped(&h).await;
        orchestrator_calls(&h, action, &sub.id()).await;

        let question = asked(&h).await;
        h.session
            .questions
            .resolve(&question.id, QuestionAnswer::choice(1));

        let told = told(&h).await;
        assert!(
            told.contains("the user chose to keep tests"),
            "{action}: {told}"
        );
        stays_stopped(&h, &sub).await;
        gate.add_permits(1);
        h.core.shutdown().await;
    }
}

/// The orchestrator is interrupted while the user is being asked: the
/// question goes with the turn, and the agent stays stopped.
#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_question_leaves_the_agent_stopped() {
    let h = Harness::new().await;
    let (gate, sub) = user_stopped(&h).await;
    orchestrator_calls(&h, "send", &sub.id()).await;
    let question = asked(&h).await;

    h.session.interrupt();
    wait_for("the interrupted turn to end", || {
        h.main_turns_ended() >= 2 && !h.main().is_running()
    })
    .await;
    assert!(h.session.questions.get(&question.id).is_none());
    stays_stopped(&h, &sub).await;
    gate.add_permits(1);
    h.core.shutdown().await;
}

/// The user stops the agent again while the question is open: the "Resume"
/// they then give was for the stop before, and does not lift this one.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_made_while_the_user_was_asked_is_not_lifted() {
    for action in ["send", "continue"] {
        let h = Harness::new().await;
        let (gate, sub) = user_stopped(&h).await;
        orchestrator_calls(&h, action, &sub.id()).await;
        let question = asked(&h).await;

        h.session.stop_agent(&sub.id()).await.unwrap();
        h.session
            .questions
            .resolve(&question.id, QuestionAnswer::choice(0));

        let told = told(&h).await;
        assert!(
            told.contains("stopped again while the user was being asked"),
            "{action}: {told}"
        );
        stays_stopped(&h, &sub).await;
        gate.add_permits(1);
        h.core.shutdown().await;
    }
}
