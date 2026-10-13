//! The windows a message can land in while a subagent finishes: just before
//! its seal, just after it, after its report went out unread, and a send that
//! is dropped half-way through the resume.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::Context;
use std::time::Duration;

use futures::FutureExt;
use pacode_tools::SendOutcome;
use pacode_types::{AgentStatus, Event};
use serde_json::json;
use tokio::sync::Semaphore;

use super::harness::*;
use super::seal_hooks::{self, Point};

/// A message landing between the subagent's last step and its seal re-drives
/// it — and its card never says `Finished` in between.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_just_before_the_seal_redrives_without_a_finished_flash() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("first")).await;
    let sub = h.sub();
    let outcome = Arc::new(Mutex::new(None));
    {
        let (session, main, id) = (h.session.clone(), h.main(), sub.id());
        let outcome = outcome.clone();
        seal_hooks::install(sub.id(), Point::BeforeSeal, move || {
            let sent = super::send(&session, &main, &id, "one more thing").now_or_never();
            *outcome.lock().unwrap() = sent;
        });
    }
    h.provider.script(true, vec![text("second, with the note")]);
    gate.add_permits(1);

    wait_for("the second turn to finish", || {
        h.provider.seen(true).len() == 2 && sub.status() == AgentStatus::Finished
    })
    .await;
    assert!(
        matches!(
            outcome.lock().unwrap().take(),
            Some(Ok(SendOutcome::Delivered))
        ),
        "the message reached the turn before its seal"
    );
    assert!(
        h.provider.seen(true)[1]
            .contains("<orchestrator_message>\none more thing\n</orchestrator_message>")
    );
    let events = h.events();
    let second_turn = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Event::TurnStarted { agent, .. } if *agent == sub.id()))
        .nth(1)
        .map(|(index, _)| index)
        .expect("a second turn");
    assert!(
        !events[..second_turn].iter().any(|e| matches!(
            e,
            Event::AgentUpdated(info) if info.id == sub.id() && info.status == AgentStatus::Finished
        )),
        "no Finished before the re-drive"
    );
    h.core.shutdown().await;
}

/// A message landing after the seal waits out the finalization, resumes the
/// agent, and takes back the report the finalization just sent — the
/// orchestrator is busy and has not read it.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_just_after_the_seal_resumes_and_takes_back_the_report() {
    let h = Harness::new().await;
    let sub_gate = Arc::new(Semaphore::new(0));
    h.provider.script(
        false,
        vec![
            tool(
                "agent",
                json!({"action": "spawn", "name": "tests", "prompt": "work"}),
            ),
            // The orchestrator stays busy, so the report waits unread.
            tool("gate", json!({})),
            text("ok"),
        ],
    );
    h.provider.script(
        true,
        vec![
            gated(&sub_gate, text("first report")),
            text("second report"),
        ],
    );
    h.session.submit_user_message("go".into()).await.unwrap();
    h.gate_entered().await;
    let sub = h.sub();
    let resumed = Arc::new(Mutex::new(None));
    {
        let (session, main, id) = (h.session.clone(), h.main(), sub.id());
        let resumed = resumed.clone();
        seal_hooks::install(sub.id(), Point::AfterSeal, move || {
            let mut send =
                Box::pin(async move { super::send(&session, &main, &id, "now part two").await });
            // One poll finds the turn sealed and queues on its lock...
            let waker = futures::task::noop_waker();
            assert!(
                send.as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending()
            );
            // ...and a task carries it on once the lock is released.
            *resumed.lock().unwrap() = Some(tokio::spawn(send));
        });
    }
    sub_gate.add_permits(1);

    wait_for("the seal", || resumed.lock().unwrap().is_some()).await;
    let handle = resumed.lock().unwrap().take().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        outcome,
        SendOutcome::Resumed {
            previous_report: Some("first report".into())
        }
    );
    h.gate.release.add_permits(1);
    wait_for("the second report", || {
        h.provider
            .seen(false)
            .iter()
            .any(|r| r.contains("second report"))
    })
    .await;
    assert!(
        !h.provider
            .seen(false)
            .iter()
            .any(|r| r.contains("first report")),
        "the first report never arrives after the resume"
    );
    let resumed_request = h.provider.seen(true).last().unwrap().clone();
    let first = resumed_request.find("first report").unwrap();
    let message = resumed_request.find("now part two").unwrap();
    assert!(first < message, "{resumed_request}");
    h.core.shutdown().await;
}

/// The report went out before the message, while the orchestrator was busy:
/// queued, or parked by the notice lock. Either way the resume takes it back.
#[tokio::test(flavor = "multi_thread")]
async fn resuming_takes_back_an_unread_report() {
    for parked in [false, true] {
        let h = Harness::new().await;
        h.provider.script(
            false,
            vec![
                tool(
                    "agent",
                    json!({"action": "spawn", "name": "tests", "prompt": "work"}),
                ),
                tool("gate", json!({})),
            ],
        );
        h.provider
            .script(true, vec![text("first report"), text("second report")]);
        if parked {
            h.session.set_notice_lock(true).await;
        }
        h.session.submit_user_message("go".into()).await.unwrap();
        h.gate_entered().await;
        let sub = h.sub();
        wait_for("the first report", || {
            sub.status() == AgentStatus::Finished && !sub.is_running()
        })
        .await;
        if parked {
            assert_eq!(h.main().notice_lock.held(), 1);
        }

        let outcome = super::send(&h.session, &h.main(), &sub.id(), "part two")
            .await
            .unwrap();
        assert_eq!(
            outcome,
            SendOutcome::Resumed {
                previous_report: Some("first report".into())
            },
            "parked={parked}"
        );
        assert_eq!(h.main().notice_lock.held(), 0, "parked={parked}");
        h.gate.release.add_permits(1);
        h.core.shutdown().await;
    }
}

/// The orchestrator is interrupted while its `send` is resuming a finished
/// subagent: the call is gone, the resume is not left half-done.
#[tokio::test(flavor = "multi_thread")]
async fn a_send_dropped_mid_resume_still_resumes_the_subagent() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("first report")).await;
    gate.add_permits(1);
    let sub = h.sub();
    wait_for("the first report to be read", || {
        sub.status() == AgentStatus::Finished
            && h.provider
                .seen(false)
                .iter()
                .any(|r| r.contains("first report"))
    })
    .await;
    wait_for("the orchestrator to settle", || h.main_idle()).await;
    h.provider.script(true, vec![text("second report")]);

    // One poll takes the turn lock and starts the resume; then the call is
    // dropped, as an interrupted orchestrator's tool call is.
    let (main, id) = (h.main(), sub.id());
    let mut send = Box::pin(super::send(&h.session, &main, &id, "part two"));
    let waker = futures::task::noop_waker();
    assert!(
        send.as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    drop(send);

    wait_for("the resumed turn", || h.provider.seen(true).len() == 2).await;
    let resumed = &h.provider.seen(true)[1];
    assert_eq!(resumed.matches("<orchestrator_message>").count(), 1);
    wait_for("the second report", || {
        sub.status() == AgentStatus::Finished && !sub.is_running()
    })
    .await;
    h.core.shutdown().await;
}
