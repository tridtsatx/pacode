//! The edges of the exchange: turns that end early, stops, detach, store
//! failures and the windows around a turn's seal.

use pacode_types::AgentStatus;
use serde_json::json;

use super::harness::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_steer_that_preempts_the_parent_turn_leaves_the_question_open() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which db?"}))).await;
    h.provider.script(
        false,
        vec![
            // The turn that is shown the question parks in a tool call...
            tool("gate", json!({})),
            // ...and the turn the user's message starts answers it.
            send_to("<agent_question", "postgres"),
            text("done"),
        ],
    );
    h.provider.script(true, vec![text("used postgres")]);
    gate.add_permits(1);
    h.gate_entered().await;
    // The user types while the tool runs: the admission preempts the turn at
    // its next step, before the model has seen the tool's result.
    h.session
        .submit_user_message("also, hurry".into())
        .await
        .unwrap();
    h.gate.release.add_permits(1);

    let sub = h.sub();
    wait_for("the subagent to finish", || {
        sub.status() == AgentStatus::Finished
    })
    .await;
    let sub_seen = h.provider.seen(true);
    assert!(
        sub_seen[1].contains("The orchestrator answered:\n\npostgres"),
        "{sub_seen:#?}"
    );
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parent_turn_that_fails_after_seeing_the_question_resolves_it() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which db?"}))).await;
    h.provider.script(false, vec![fail("provider exploded")]);
    h.provider
        .script(true, vec![text("went with my best guess")]);
    gate.add_permits(1);

    let sub = h.sub();
    wait_for("the subagent to finish", || {
        sub.status() == AgentStatus::Finished
    })
    .await;
    let sub_seen = h.provider.seen(true);
    assert!(
        sub_seen[1].contains(pacode_tools::builtin::ask_parent::PARENT_FAILED),
        "{sub_seen:#?}"
    );
    h.core.shutdown().await;
}

/// The orchestrator's turn cannot put the question into its history (the
/// store fails): the question fails instead of vanishing with the turn and
/// leaving its subagent blocked forever.
#[tokio::test(flavor = "multi_thread")]
async fn a_question_that_cannot_be_delivered_fails_instead_of_hanging() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("unused")).await;
    let sub = h.sub();
    let main = h.main();
    let opened = sub
        .mailbox
        .open(
            &main,
            crate::inject::Injection::AgentQuestion {
                agent: sub.id(),
                name: "tests".into(),
                call_id: pacode_types::CallId::new("call_0"),
                question: "which db?".into(),
            },
        )
        .unwrap();
    let super::mailbox::Opened::Asked(reply) = opened else {
        panic!("the question was not sent");
    };
    h.session.store.set_fault_injection(true);
    crate::turn::lifecycle::request_turn(h.session.clone(), main);
    let reply = tokio::time::timeout(std::time::Duration::from_secs(10), reply)
        .await
        .expect("the question was left waiting")
        .unwrap();
    h.session.store.set_fault_injection(false);
    assert_eq!(reply.reply, pacode_tools::ParentReply::ParentFailed);
    gate.add_permits(1);
    h.core.shutdown().await;
}

/// The turn that was shown the question is detached into a background agent,
/// which cannot answer (no `agent` tool): the new orchestrator is shown the
/// question and answers it.
#[tokio::test(flavor = "multi_thread")]
async fn a_detach_hands_the_open_question_to_the_new_orchestrator() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which db?"}))).await;
    let sub = h.sub();
    h.provider.script(
        false,
        vec![
            tool("gate", json!({})),
            send_to("<agent_question", "postgres"),
            text("done"),
        ],
    );
    h.provider.script(true, vec![text("used postgres")]);
    gate.add_permits(1);
    h.gate_entered().await;
    h.session.detach_main_turn().await.unwrap();

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

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_the_user_stopped_is_not_resumed_by_send() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("partial")).await;
    let sub = h.sub();
    h.session.stop_agent(&sub.id()).await.unwrap();
    wait_for("the stop", || !sub.is_running()).await;

    let err = super::send(&h.session, &h.main(), &sub.id(), "go on")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("stopped by the user"), "{err}");
    // `agent continue` follows the same rule.
    let host = crate::host::SessionHost {
        session: h.session.clone(),
        agent: pacode_types::AgentId::main(),
    };
    let err = pacode_tools::ToolHost::continue_agent(&host, &sub.id())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("stopped by the user"), "{err}");
    assert_eq!(sub.status(), AgentStatus::Stopped);
    gate.add_permits(1);
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_the_orchestrator_stopped_is_resumed_by_send() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("partial")).await;
    let sub = h.sub();
    h.session
        .stop_agent_by(&sub.id(), super::StopOrigin::Orchestrator)
        .await
        .unwrap();
    wait_for("the stop", || !sub.is_running()).await;
    h.provider.script(true, vec![text("resumed work")]);

    let outcome = super::send(&h.session, &h.main(), &sub.id(), "go on")
        .await
        .unwrap();
    assert_eq!(
        outcome,
        pacode_tools::SendOutcome::Resumed {
            previous_report: None
        }
    );
    wait_for("the resumed turn", || {
        h.provider
            .seen(true)
            .iter()
            .any(|r| r.contains("<orchestrator_message>\ngo on"))
    })
    .await;
    gate.add_permits(1);
    h.core.shutdown().await;
}

/// An orchestrator that keeps not answering is not woken for a fourth
/// question: the subagent is told to go on by itself.
#[tokio::test(flavor = "multi_thread")]
async fn after_three_unanswered_questions_the_next_is_not_sent() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "q1?"}))).await;
    h.provider.script(
        true,
        vec![
            tool("ask_parent", json!({"question": "q2?"})),
            tool("ask_parent", json!({"question": "q3?"})),
            tool("ask_parent", json!({"question": "q4?"})),
            text("gave up asking"),
        ],
    );
    gate.add_permits(1);

    let sub = h.sub();
    wait_for("the subagent to finish", || {
        sub.status() == AgentStatus::Finished
    })
    .await;
    let sub_seen = h.provider.seen(true);
    assert!(
        sub_seen[4].contains(pacode_tools::builtin::ask_parent::NOT_ANSWERING),
        "{sub_seen:#?}"
    );
    let main_seen = h.provider.seen(false);
    assert!(main_seen.iter().any(|r| r.contains("q3?")));
    assert!(
        !main_seen.iter().any(|r| r.contains("q4?")),
        "{main_seen:#?}"
    );
    h.core.shutdown().await;
}
