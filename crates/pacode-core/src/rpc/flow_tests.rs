//! End to end: a scripted orchestrator and scripted subagents exchanging
//! `agent send` / `ask_parent` through a real core, store and turn loop.

use std::sync::Arc;

use pacode_types::{AgentId, AgentReply, AgentStatus};
use serde_json::json;
use tokio::sync::Semaphore;

use super::harness::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_question_is_answered_by_agent_send_and_the_report_follows() {
    let h = Harness::new().await;
    let gate = spawn_parked(
        &h,
        tool("ask_parent", json!({"question": "sqlite or postgres?"})),
    )
    .await;
    h.provider.script(
        false,
        vec![send_to("<agent_question", "use postgres"), text("ok")],
    );
    h.provider.script(true, vec![text("report: used postgres")]);
    gate.add_permits(1);

    let sub = h.sub();
    wait_for("the subagent's report", || {
        sub.status() == AgentStatus::Finished
            && h.provider
                .seen(false)
                .iter()
                .any(|r| r.contains("report: used postgres"))
    })
    .await;
    wait_for("the orchestrator to settle", || h.main_idle()).await;

    let main_seen = h.provider.seen(false);
    assert!(
        main_seen
            .iter()
            .any(|r| r.contains("<agent_question") && r.contains("sqlite or postgres?")),
        "{main_seen:#?}"
    );
    assert!(main_seen.iter().any(|r| r.contains("answered it")));
    let sub_seen = h.provider.seen(true);
    assert!(
        sub_seen[1].contains("The orchestrator answered:\n\nuse postgres"),
        "{sub_seen:#?}"
    );
    assert!(h.saw_status(&sub.id(), AgentStatus::WaitingParent));
    let replies = h.question_replies();
    assert_eq!(replies.len(), 2, "one item per transcript: {replies:?}");
    for (_, reply) in replies {
        assert_eq!(
            reply,
            Some(AgentReply::Answered {
                text: "use postgres".into()
            })
        );
    }
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_completed_turn_without_an_answer_lets_the_subagent_proceed() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which?"}))).await;
    h.provider.script(false, vec![text("not answering")]);
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
        sub_seen[1].contains(pacode_tools::builtin::ask_parent::UNANSWERED),
        "{sub_seen:#?}"
    );
    for (_, reply) in h.question_replies() {
        assert_eq!(reply, Some(AgentReply::Unanswered));
    }
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_parent_turn_leaves_the_question_open() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which?"}))).await;
    // The orchestrator's turn that sees the question never gets an answer
    // from the model: the user interrupts it.
    let never = Arc::new(Semaphore::new(0));
    h.provider
        .script(false, vec![gated(&never, text("unreachable"))]);
    gate.add_permits(1);

    let sub = h.sub();
    wait_for("the question to reach the orchestrator", || {
        h.provider
            .seen(false)
            .iter()
            .any(|r| r.contains("<agent_question"))
    })
    .await;
    h.session.interrupt();
    wait_for("the interrupted turn to end", || h.main_turns_ended() >= 2).await;
    // A turn settles its questions before it announces its end, so by now a
    // resolution would have taken the question out of the mailbox.
    assert!(sub.mailbox.has_open_question());
    assert_eq!(sub.status(), AgentStatus::WaitingParent);

    // The next completed turn answers it after all.
    h.provider
        .script(false, vec![send_to("<agent_question", "late answer")]);
    h.provider.script(true, vec![text("done")]);
    h.session
        .submit_user_message("answer it".into())
        .await
        .unwrap();
    wait_for("the subagent to finish", || {
        sub.status() == AgentStatus::Finished
    })
    .await;
    assert!(h.provider.seen(true)[1].contains("late answer"));
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_waiting_subagent_cancels_its_question() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, tool("ask_parent", json!({"question": "which?"}))).await;
    h.provider
        .script(false, vec![stop_asker(), text("stopped it")]);
    gate.add_permits(1);

    let sub = h.sub();
    wait_for("the stop to land", || {
        sub.status() == AgentStatus::Stopped && !sub.is_running()
    })
    .await;
    wait_for("the orchestrator to settle", || h.main_idle()).await;
    let replies = h.question_replies();
    assert_eq!(replies.len(), 2);
    for (_, reply) in replies {
        assert_eq!(reply, Some(AgentReply::Cancelled));
    }
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_questions_one_answered_one_left_to_the_subagent() {
    let h = Harness::new().await;
    let gate = Arc::new(Semaphore::new(0));
    h.provider.script(
        false,
        vec![
            tool(
                "agent",
                json!({"action": "spawn", "name": "a", "prompt": "work"}),
            ),
            tool(
                "agent",
                json!({"action": "spawn", "name": "b", "prompt": "work"}),
            ),
            text("spawned"),
        ],
    );
    h.provider.script(
        true,
        vec![
            gated(&gate, tool("ask_parent", json!({"question": "q?"}))),
            gated(&gate, tool("ask_parent", json!({"question": "q?"}))),
        ],
    );
    h.session.submit_user_message("go".into()).await.unwrap();
    wait_for("the first orchestrator turn", || h.main_turns_ended() >= 1).await;
    wait_for("both subagents", || h.subs().len() == 2).await;
    // Answer whichever question shows up first; the turn then ends.
    h.provider.script(
        false,
        vec![send_to("<agent_question", "answer"), text("done")],
    );
    gate.add_permits(2);

    let subs = h.subs();
    wait_for("both subagents to finish", || {
        subs.iter().all(|s| s.status() == AgentStatus::Finished)
    })
    .await;
    let replies = h.question_replies();
    let answered = replies
        .iter()
        .filter(|(_, r)| matches!(r, Some(AgentReply::Answered { .. })))
        .count();
    let unanswered = replies
        .iter()
        .filter(|(_, r)| *r == Some(AgentReply::Unanswered))
        .count();
    assert_eq!((answered, unanswered), (2, 2), "{replies:?}");
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_to_a_running_subagent_is_read_at_its_next_step() {
    let h = Harness::new().await;
    // The subagent sits in its first model call while the orchestrator sends.
    let gate = spawn_parked(&h, text("partial")).await;
    h.provider
        .script(true, vec![text("final, with the orchestrator's note")]);
    let sub = h.sub();
    let reply = crate::rpc::send(
        &h.session,
        &h.session.main_agent().unwrap(),
        &sub.id(),
        "also cover the edge case",
        crate::rpc::Consent::Absent,
    )
    .await
    .unwrap();
    assert_eq!(reply, pacode_tools::SendOutcome::Delivered);
    gate.add_permits(1);

    wait_for("the subagent to finish", || {
        sub.status() == AgentStatus::Finished
    })
    .await;
    let sub_seen = h.provider.seen(true);
    assert_eq!(sub_seen.len(), 2, "{sub_seen:#?}");
    assert!(
        sub_seen[1]
            .contains("<orchestrator_message>\nalso cover the edge case\n</orchestrator_message>"),
        "{sub_seen:#?}"
    );
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_to_a_finished_subagent_resumes_it_with_its_context() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("first report")).await;
    gate.add_permits(1);
    let sub = h.sub();
    wait_for("the first report", || {
        sub.status() == AgentStatus::Finished
            && h.provider
                .seen(false)
                .iter()
                .any(|r| r.contains("first report"))
    })
    .await;
    wait_for("the orchestrator to settle", || h.main_idle()).await;

    h.provider.script(
        false,
        vec![send_to("<agent_finished", "now do part two"), text("sent")],
    );
    h.provider.script(true, vec![text("second report")]);
    h.session
        .submit_user_message("continue it".into())
        .await
        .unwrap();
    wait_for("the second report", || {
        h.provider
            .seen(false)
            .iter()
            .any(|r| r.contains("second report"))
    })
    .await;

    let main_seen = h.provider.seen(false);
    assert!(main_seen.iter().any(|r| r.contains("resumed it")));
    let sub_seen = h.provider.seen(true);
    let resumed = sub_seen.last().unwrap();
    // The reloaded history: the brief, the first report, then the message.
    let brief = resumed.find("work").unwrap();
    let first = resumed.find("first report").unwrap();
    let message = resumed.find("<orchestrator_message>").unwrap();
    assert!(brief < first && first < message, "{resumed}");
    wait_for("the subagent to finish again", || {
        sub.status() == AgentStatus::Finished && !sub.is_running()
    })
    .await;
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_subagent_cannot_be_resumed() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("report")).await;
    gate.add_permits(1);
    let sub = h.sub();
    wait_for("the report", || sub.status() == AgentStatus::Finished).await;
    wait_for("the orchestrator to settle", || h.main_idle()).await;
    sub.set_failed(None, "provider exploded".into());

    let err = crate::rpc::send(
        &h.session,
        &h.session.main_agent().unwrap(),
        &sub.id(),
        "try again",
        crate::rpc::Consent::Absent,
    )
    .await
    .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("cannot be resumed") && text.contains("provider exploded"),
        "{text}"
    );
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_parent_may_message_a_subagent() {
    let h = Harness::new().await;
    let gate = spawn_parked(&h, text("report")).await;
    let sub = h.sub();
    let err = crate::rpc::send(
        &h.session,
        &sub,
        &sub.id(),
        "hi",
        crate::rpc::Consent::Absent,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("not your subagent"), "{err}");
    let err = crate::rpc::send(
        &h.session,
        &h.session.main_agent().unwrap(),
        &AgentId::new("agt_missing"),
        "hi",
        crate::rpc::Consent::Absent,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    gate.add_permits(1);
    h.core.shutdown().await;
}

/// A subagent caught in a tool loop ends at `agents.max_turns` steps: its
/// turn stops with a notice on its card, it lands `Finished`, and the report
/// the orchestrator reads says it was cut short.
#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_in_a_tool_loop_stops_at_max_turns_and_says_so() {
    let mut config = pacode_types::Config::default();
    config.agents.max_turns = 2;
    let h = Harness::with_config(config).await;
    // Every `gate` call the loop makes passes straight through.
    h.gate.release.add_permits(16);
    let gate = spawn_parked(&h, tool("gate", json!({}))).await;
    h.provider
        .script(true, (0..5).map(|_| tool("gate", json!({}))).collect());
    h.provider.script(false, vec![text("noted")]);
    gate.add_permits(1);

    let sub = h.sub();
    wait_for("the capped subagent to finish", || {
        sub.status() == AgentStatus::Finished
    })
    .await;
    assert_eq!(h.provider.seen(true).len(), 2, "two steps, then the cap");
    wait_for("the report", || {
        h.provider
            .seen(false)
            .iter()
            .any(|r| r.contains("<agent_finished") && r.contains("`agents.max_turns`"))
    })
    .await;
    assert!(h.events().iter().any(|event| matches!(
        event,
        pacode_types::Event::ItemAdded(item)
            if item.agent == sub.id()
                && matches!(
                    &item.kind,
                    pacode_types::TranscriptKind::Notice { text, .. }
                        if text.contains("maximum turns limit (2)")
                )
    )));
    h.core.shutdown().await;
}

/// A resume makes an ended subagent live again, so it takes a slot under
/// `agents.max_live` like a spawn: refused, with the limit named, while the
/// only slot is taken.
#[tokio::test(flavor = "multi_thread")]
async fn resuming_a_finished_subagent_respects_the_live_cap() {
    let mut config = pacode_types::Config::default();
    config.agents.max_live = 1;
    let h = Harness::with_config(config).await;
    let gate = spawn_parked(&h, text("first done")).await;
    h.provider.script(false, vec![text("ok")]);
    gate.add_permits(1);
    let first = h.sub();
    wait_for("the first subagent to finish", || {
        first.status() == AgentStatus::Finished
    })
    .await;
    wait_for("the orchestrator to settle", || h.main_idle()).await;

    let parked = Arc::new(Semaphore::new(0));
    h.provider
        .script(true, vec![gated(&parked, text("second done"))]);
    h.session
        .spawn_agent(pacode_tools::AgentSpec {
            name: Some("second".into()),
            prompt: "work".into(),
            tools: None,
            exclude_tools: None,
            fork: false,
            model: None,
            effort: None,
        })
        .await
        .expect("the slot the first one freed");

    let err = super::send(
        &h.session,
        &h.main(),
        &first.id(),
        "one more thing",
        super::Consent::Absent,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("maximum live subagents (1)"),
        "{err}"
    );
    assert_eq!(first.status(), AgentStatus::Finished);
    parked.add_permits(1);
    h.core.shutdown().await;
}
