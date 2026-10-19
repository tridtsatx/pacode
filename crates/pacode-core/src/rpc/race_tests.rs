//! Races at the seams of the exchange: a stop, a close or a detach landing in
//! the middle of a resume, a `continue` or a question, and an answer the stop
//! beat to the asking call.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::Context;

use pacode_tools::SendOutcome;
use pacode_types::{AgentReply, AgentStatus, ContentBlock};
use serde_json::json;

use super::StopOrigin;
use super::harness::*;
use super::hooks::{self, Point, block_until, race};

/// A user stop that lands while a resume is persisting the message wins: the
/// resume does not take effect, and the stop stays the user's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_stop_during_a_resume_wins_and_stays_the_users() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("partial")).await;
    let sub = h.sub();
    h.session
        .stop_agent_by(&sub.id(), StopOrigin::Orchestrator)
        .await
        .unwrap();
    wait_for("the stop", || !sub.is_running()).await;
    {
        let (session, sub) = (h.session.clone(), sub.clone());
        hooks::install(sub.id(), Point::ResumeCommitted, move || {
            let id = sub.id();
            // Its first step records the origin and moves the generation;
            // the rest waits for the message lock the resume holds.
            race(async move { session.stop_agent(&id).await });
            block_until("the user stop", 10, || {
                sub.mailbox.stopped_by() == Some(StopOrigin::User)
            });
        });
    }

    let err = super::send(
        &h.session,
        &h.main(),
        &sub.id(),
        "go on",
        super::Consent::Absent,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("stopped by the user"), "{err}");
    wait_for("the user stop to land", || {
        sub.status() == AgentStatus::Stopped && !sub.is_running()
    })
    .await;
    assert_eq!(sub.mailbox.stopped_by(), Some(StopOrigin::User));
    let err = super::send(
        &h.session,
        &h.main(),
        &sub.id(),
        "go on",
        super::Consent::Absent,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("stopped by the user"), "{err}");
    gate.add_permits(1);
    h.core.shutdown().await;
}

/// A close that lands after a resume checked the session is open, but before
/// it persisted the message: the resume yields, no turn runs on the closing
/// session, and the close is not left waiting behind one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_the_session_during_a_resume_starts_no_turn() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("partial")).await;
    let sub = h.sub();
    h.session
        .stop_agent_by(&sub.id(), StopOrigin::Orchestrator)
        .await
        .unwrap();
    wait_for("the stop", || !sub.is_running()).await;
    let requests_before = h.provider.seen(true).len();
    let close = Arc::new(Mutex::new(None));
    {
        let (core, session, sub, close) = (
            h.core.clone(),
            h.session.clone(),
            sub.clone(),
            close.clone(),
        );
        hooks::install(sub.id(), Point::ResumeChecked, move || {
            let generation = sub.turn_generation();
            let id = session.id.clone();
            *close.lock().unwrap() = Some(race(async move { core.close_session(&id).await }));
            block_until("the close to stop the agent", 10, || {
                sub.turn_generation() != generation
            });
        });
    }

    let err = super::send(
        &h.session,
        &h.main(),
        &sub.id(),
        "go on",
        super::Consent::Absent,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("closed"), "{err}");
    let close = close.lock().unwrap().take().unwrap();
    wait_for("the close", || close.is_finished()).await;
    assert_eq!(h.provider.seen(true).len(), requests_before);
    gate.add_permits(1);
    h.core.shutdown().await;
}

/// A message sent while the orchestrator's own stop of the agent is still
/// under way waits the stop out and resumes the agent from it: a stop that
/// began first is not one that lands during the resume.
#[tokio::test(flavor = "multi_thread")]
async fn a_send_behind_the_orchestrators_stop_resumes_the_agent_after_it() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("first report")).await;
    gate.add_permits(1);
    let sub = h.sub();
    wait_for("the first report", || {
        sub.status() == AgentStatus::Finished && !sub.is_running()
    })
    .await;
    wait_for("the orchestrator to settle", || h.main_idle()).await;
    h.provider.script(true, vec![text("second report")]);

    // The stop is in — recorded, generation moved — and queued on the
    // message lock, ahead of anything the send does.
    let held = sub.message_write_lock.clone().lock_owned().await;
    let id = sub.id();
    let mut stop = Box::pin(h.session.stop_agent_by(&id, StopOrigin::Orchestrator));
    let waker = futures::task::noop_waker();
    assert!(
        stop.as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    let send = tokio::spawn({
        let (session, main, id) = (h.session.clone(), h.main(), id.clone());
        async move { super::send(&session, &main, &id, "part two", super::Consent::Absent).await }
    });
    drop(held);
    stop.await.unwrap();

    assert_eq!(
        send.await.unwrap().unwrap(),
        SendOutcome::Resumed {
            previous_report: None
        }
    );
    wait_for("the resumed turn", || {
        h.provider
            .seen(true)
            .iter()
            .any(|r| r.contains("<orchestrator_message>\npart two"))
    })
    .await;
    h.core.shutdown().await;
}

/// The orchestrator's `continue` races a user stop the same way a resume does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_stop_during_continue_wins_and_stays_the_users() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("partial")).await;
    let sub = h.sub();
    h.session
        .stop_agent_by(&sub.id(), StopOrigin::Orchestrator)
        .await
        .unwrap();
    wait_for("the stop", || !sub.is_running()).await;
    {
        let (session, sub) = (h.session.clone(), sub.clone());
        hooks::install(sub.id(), Point::ContinueCommitted, move || {
            let id = sub.id();
            race(async move { session.stop_agent(&id).await });
            block_until("the user stop", 10, || {
                sub.mailbox.stopped_by() == Some(StopOrigin::User)
            });
        });
    }

    let host = crate::host::SessionHost {
        session: h.session.clone(),
        agent: pacode_types::AgentId::main(),
    };
    assert!(
        pacode_tools::ToolHost::continue_agent(&host, &sub.id(), None)
            .await
            .is_err()
    );
    wait_for("the user stop to land", || {
        sub.status() == AgentStatus::Stopped && !sub.is_running()
    })
    .await;
    assert_eq!(sub.mailbox.stopped_by(), Some(StopOrigin::User));
    let err = pacode_tools::ToolHost::continue_agent(&host, &sub.id(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("stopped by the user"), "{err}");
    gate.add_permits(1);
    h.core.shutdown().await;
}

/// A question asked while the orchestrator is being detached ends up with
/// the new orchestrator, which answers it — it is never stranded on the old
/// one, which can no longer answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_asked_during_a_detach_reaches_the_new_orchestrator() {
    let h = Harness::new().await;
    let sub_gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which db?"}))).await;
    let sub = h.sub();
    // The orchestrator is mid-turn, so it can be detached.
    h.provider.script(
        false,
        vec![
            tool("gate", json!({})),
            send_to("<agent_question", "postgres"),
            text("done"),
        ],
    );
    h.session
        .submit_user_message("keep going".into())
        .await
        .unwrap();
    h.gate_entered().await;
    {
        let session = h.session.clone();
        hooks::install(sub.id(), Point::AskLookedUp, move || {
            let (done, detached) = std::sync::mpsc::channel();
            race(async move {
                let _ = session.detach_main_turn().await;
                let _ = done.send(());
            });
            // Lets a detach that is free to go finish before the question is
            // queued; one that has to wait for the queueing never does.
            let _ = detached.recv_timeout(std::time::Duration::from_millis(500));
        });
    }
    h.provider.script(true, vec![text("used postgres")]);
    sub_gate.add_permits(1);

    wait_for("the subagent to finish", || {
        sub.status() == AgentStatus::Finished
    })
    .await;
    assert!(
        h.provider.seen(true)[1].contains("The orchestrator answered:\n\npostgres"),
        "{:#?}",
        h.provider.seen(true)
    );
    h.gate.release.add_permits(1);
    h.core.shutdown().await;
}

/// Finish a send polled once by hand and the stop polled after it, together:
/// the stop holds the agent's message lock across its last await, which a
/// send that goes on to resume the agent waits for.
async fn drive<S, T>(
    first: std::task::Poll<S>,
    send: std::pin::Pin<Box<impl Future<Output = S>>>,
    stop: std::pin::Pin<Box<impl Future<Output = Result<(), T>>>>,
) -> S
where
    T: std::fmt::Debug,
{
    match first {
        std::task::Poll::Ready(sent) => {
            stop.await.unwrap();
            sent
        }
        std::task::Poll::Pending => {
            let (sent, stopped) = tokio::join!(send, stop);
            stopped.unwrap();
            sent
        }
    }
}

/// The last tool result in `agent`'s history.
fn last_tool_result(agent: &crate::agent::Agent) -> String {
    let history = agent.history.lock().unwrap();
    history
        .messages
        .iter()
        .rev()
        .flat_map(|m| m.content.iter())
        .find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("a tool result")
}

/// An answer the stop beat to the asking call never took effect: the
/// orchestrator is not told it answered, and the question, the call's result
/// and the subagent's history all say the same — cancelled.
///
/// One thread, polled by hand, so nothing runs between the answer and the
/// stop: the asking call is cancelled before it is ever polled again.
#[tokio::test]
async fn an_answer_the_stop_beat_is_not_an_answer() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which db?"}))).await;
    // The orchestrator turn that is shown the question parks.
    h.provider.script(false, vec![tool("gate", json!({}))]);
    gate.add_permits(1);
    let sub = h.sub();
    wait_for("the question", || sub.mailbox.has_open_question()).await;
    h.gate_entered().await;

    let (main, id) = (h.main(), sub.id());
    let waker = futures::task::noop_waker();
    let mut send = Box::pin(super::send(
        &h.session,
        &main,
        &id,
        "postgres",
        super::Consent::Absent,
    ));
    let first = send.as_mut().poll(&mut Context::from_waker(&waker));
    let mut stop = Box::pin(h.session.stop_agent(&id));
    let _ = stop.as_mut().poll(&mut Context::from_waker(&waker));
    let sent = drive(first, send, stop).await;
    wait_for("the stop", || !sub.is_running()).await;

    let err = sent.expect_err("an answer the asking call never took is not an answer");
    assert!(err.to_string().contains("stopped by the user"), "{err}");
    for (_, reply) in h.question_replies() {
        assert_eq!(reply, Some(AgentReply::Cancelled));
    }
    assert_eq!(last_tool_result(&sub), "cancelled");
    h.gate.release.add_permits(1);
    h.core.shutdown().await;
}

/// The same race with a stop the orchestrator made: the message is not lost
/// with the cancelled call — it resumes the agent, as any message to an agent
/// the orchestrator stopped does.
#[tokio::test]
async fn an_answer_the_orchestrators_stop_beat_resumes_the_agent() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which db?"}))).await;
    h.provider.script(false, vec![tool("gate", json!({}))]);
    gate.add_permits(1);
    let sub = h.sub();
    wait_for("the question", || sub.mailbox.has_open_question()).await;
    h.gate_entered().await;
    h.provider.script(true, vec![text("used postgres")]);

    let (main, id) = (h.main(), sub.id());
    let waker = futures::task::noop_waker();
    let mut send = Box::pin(super::send(
        &h.session,
        &main,
        &id,
        "postgres",
        super::Consent::Absent,
    ));
    let first = send.as_mut().poll(&mut Context::from_waker(&waker));
    let mut stop = Box::pin(h.session.stop_agent_by(&id, StopOrigin::Orchestrator));
    let _ = stop.as_mut().poll(&mut Context::from_waker(&waker));
    let sent = drive(first, send, stop).await;

    assert_eq!(
        sent.unwrap(),
        SendOutcome::Resumed {
            previous_report: None
        }
    );
    wait_for("the resumed turn", || {
        h.provider
            .seen(true)
            .iter()
            .any(|r| r.contains("<orchestrator_message>\npostgres"))
    })
    .await;
    for (_, reply) in h.question_replies() {
        assert_eq!(reply, Some(AgentReply::Cancelled));
    }
    h.gate.release.add_permits(1);
    h.core.shutdown().await;
}

/// The orchestrator is interrupted while its answer is on its way: the
/// asking call takes it, and the orchestrator, whose call was dropped, is told
/// at its next step that the answer landed — so it does not send it again.
#[tokio::test]
async fn an_answer_whose_send_was_dropped_is_reported_to_the_orchestrator() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which db?"}))).await;
    h.provider
        .script(false, vec![tool("gate", json!({})), text("noted")]);
    gate.add_permits(1);
    let sub = h.sub();
    wait_for("the question", || sub.mailbox.has_open_question()).await;
    h.gate_entered().await;
    h.provider.script(true, vec![text("used postgres")]);

    let (main, id) = (h.main(), sub.id());
    let waker = futures::task::noop_waker();
    let mut send = Box::pin(super::send(
        &h.session,
        &main,
        &id,
        "postgres",
        super::Consent::Absent,
    ));
    let _ = send.as_mut().poll(&mut Context::from_waker(&waker));
    drop(send);

    wait_for("the subagent to finish", || {
        sub.status() == AgentStatus::Finished && !sub.is_running()
    })
    .await;
    assert!(h.provider.seen(true)[1].contains("The orchestrator answered:\n\npostgres"));
    h.gate.release.add_permits(1);
    wait_for("the orchestrator to be told", || {
        h.provider
            .seen(false)
            .iter()
            .any(|r| r.contains("<send_result"))
    })
    .await;
    let told = h.provider.seen(false).last().unwrap().clone();
    assert!(told.contains("answered"), "{told}");
    h.core.shutdown().await;
}
