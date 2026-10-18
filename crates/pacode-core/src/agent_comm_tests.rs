use pacode_types::{AgentId, AgentInfo, AgentKind, AgentStatus, Effort, ModelRoute};

use super::*;
use crate::inject::{Injection, InjectionQueue, render_injections};

fn info(name: &str) -> AgentInfo {
    AgentInfo {
        id: AgentId::new("agt_tests"),
        name: name.to_string(),
        kind: AgentKind::Sub,
        status: AgentStatus::Thinking,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: Effort::Low,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    }
}

fn agent() -> AgentId {
    AgentId::new("agt_tests")
}

#[test]
fn text_is_trimmed_and_bounded() {
    assert_eq!(check_text("  hi \n").unwrap(), "hi");
    assert_eq!(check_text("   "), Err(CommError::EmptyText));
    let long = "x".repeat(MAX_MESSAGE_CHARS + 1);
    assert_eq!(
        check_text(&long),
        Err(CommError::TextTooLong {
            len: MAX_MESSAGE_CHARS + 1,
            limit: MAX_MESSAGE_CHARS
        })
    );
    assert!(check_text(&"x".repeat(MAX_MESSAGE_CHARS)).is_ok());
}

#[test]
fn only_a_live_running_agent_takes_messages() {
    assert_eq!(delivery(true, true), Delivery::Accept);
    assert_eq!(delivery(true, false), Delivery::Reject);
    assert_eq!(delivery(false, true), Delivery::Reject);
    assert_eq!(delivery(false, false), Delivery::Reject);
}

#[test]
fn only_an_idle_main_agent_is_woken() {
    assert!(should_wake(true, false));
    assert!(!should_wake(true, true));
    assert!(!should_wake(false, false));
    assert!(!should_wake(false, true));
}

#[test]
fn short_keeps_one_line() {
    assert_eq!(short("one line", 20), "one line");
    assert_eq!(short("first\nsecond", 20), "first…");
    assert_eq!(short(&"a".repeat(30), 10), format!("{}…", "a".repeat(9)));
}

#[test]
fn question_ids_are_q_numbers_and_unique() {
    let comm = AgentComm::default();
    let (a, _) = comm.begin_question(5, false).unwrap();
    comm.cancel();
    let (b, _) = comm.begin_question(5, false).unwrap();
    assert!(a.starts_with('q') && a[1..].parse::<u64>().is_ok(), "{a}");
    assert_ne!(a, b);
}

#[test]
fn one_pending_question_at_a_time() {
    let comm = AgentComm::default();
    let (first, _) = comm.begin_question(5, false).unwrap();
    assert_eq!(
        comm.begin_question(5, false).err(),
        Some(CommError::QuestionPending(first.clone()))
    );
    assert_eq!(comm.pending_id(), Some(first));
    assert_eq!(comm.asked(), 1);
}

#[test]
fn questions_are_limited_over_the_lifetime() {
    let comm = AgentComm::default();
    for _ in 0..2 {
        let (id, _) = comm.begin_question(2, false).unwrap();
        comm.answer(&agent(), &id, "ok").unwrap();
    }
    assert_eq!(
        comm.begin_question(2, false).err(),
        Some(CommError::AskLimit(2))
    );
    // A cap of zero forbids asking at all.
    assert_eq!(
        AgentComm::default().begin_question(0, true).err(),
        Some(CommError::AskLimit(0))
    );
}

#[test]
fn answer_reaches_a_blocked_waiter() {
    let comm = AgentComm::default();
    let (id, rx) = comm.begin_question(5, true).unwrap();
    let mut rx = rx.expect("waiter");
    assert_eq!(
        comm.answer(&agent(), &id, "use sqlite"),
        Ok(AnswerRoute::Waiter)
    );
    assert_eq!(
        rx.try_recv().unwrap(),
        AskOutcome::Answered("use sqlite".into())
    );
    assert_eq!(comm.pending_id(), None);
}

#[test]
fn answer_to_a_non_blocking_question_is_left_for_injection() {
    let comm = AgentComm::default();
    let (id, rx) = comm.begin_question(5, false).unwrap();
    assert!(rx.is_none());
    assert_eq!(comm.answer(&agent(), &id, "yes"), Ok(AnswerRoute::Later));
    assert_eq!(comm.pending_id(), None);
}

#[test]
fn answer_checks_the_question_id() {
    let comm = AgentComm::default();
    assert_eq!(
        comm.answer(&agent(), "q1", "x"),
        Err(CommError::NoPendingQuestion(agent()))
    );
    let (id, _rx) = comm.begin_question(5, true).unwrap();
    let err = comm.answer(&agent(), "q999999", "x").unwrap_err();
    assert_eq!(
        err,
        CommError::WrongQuestion {
            agent: agent(),
            expected: id.clone(),
            got: "q999999".into()
        }
    );
    assert_eq!(comm.pending_id(), Some(id));
}

#[test]
fn a_plain_message_answers_a_blocked_question() {
    let comm = AgentComm::default();
    let (id, rx) = comm.begin_question(5, true).unwrap();
    let mut rx = rx.expect("waiter");
    assert_eq!(comm.deliver_message("go left"), MessageRoute::Consumed(id));
    assert_eq!(
        rx.try_recv().unwrap(),
        AskOutcome::Answered("go left".into())
    );
    assert_eq!(comm.deliver_message("again"), MessageRoute::Queue);
}

#[test]
fn a_plain_message_settles_a_non_blocking_question_but_is_still_queued() {
    let comm = AgentComm::default();
    let (id, _) = comm.begin_question(5, false).unwrap();
    assert_eq!(comm.deliver_message("note"), MessageRoute::Queue);
    assert_eq!(comm.pending_id(), Some(id.clone()));
    assert_eq!(comm.clear_unwaited(), Some(id));
    assert_eq!(comm.pending_id(), None);
}

#[test]
fn clear_unwaited_leaves_a_blocked_question_alone() {
    let comm = AgentComm::default();
    let (id, _rx) = comm.begin_question(5, true).unwrap();
    assert_eq!(comm.clear_unwaited(), None);
    assert_eq!(comm.pending_id(), Some(id));
}

#[test]
fn expire_removes_only_the_matching_question() {
    let comm = AgentComm::default();
    let (id, _rx) = comm.begin_question(5, true).unwrap();
    assert!(!comm.expire("q0"));
    assert!(comm.expire(&id));
    assert!(!comm.expire(&id));
    assert_eq!(comm.pending_id(), None);
}

#[test]
fn cancel_wakes_the_waiter_with_cancelled() {
    let comm = AgentComm::default();
    assert_eq!(comm.cancel(), None);
    let (id, rx) = comm.begin_question(5, true).unwrap();
    let mut rx = rx.expect("waiter");
    assert_eq!(comm.cancel(), Some(id));
    assert_eq!(rx.try_recv().unwrap(), AskOutcome::Cancelled);
    assert_eq!(comm.pending_id(), None);
}

#[test]
fn queued_messages_are_capped_and_keep_their_order() {
    let queue = InjectionQueue::default();
    for i in 0..MAX_QUEUED_PARENT_MESSAGES {
        let ok = queue.push_limited(
            Injection::ParentMessage {
                from: "orchestrator".into(),
                text: format!("m{i}"),
            },
            MAX_QUEUED_PARENT_MESSAGES,
            is_parent_message,
        );
        assert!(ok, "message {i} should fit");
    }
    // Other injections neither count nor block.
    queue.push(Injection::SystemNotice("n".into()));
    assert!(!queue.push_limited(
        Injection::ParentMessage {
            from: "orchestrator".into(),
            text: "overflow".into()
        },
        MAX_QUEUED_PARENT_MESSAGES,
        is_parent_message,
    ));
    assert_eq!(
        queue.count_matching(is_parent_message),
        MAX_QUEUED_PARENT_MESSAGES
    );

    let drained = queue.drain();
    assert_eq!(drained.len(), MAX_QUEUED_PARENT_MESSAGES + 1);
    assert_eq!(
        drained[0],
        Injection::ParentMessage {
            from: "orchestrator".into(),
            text: "m0".into()
        }
    );
    // Room again after the agent read them.
    assert!(queue.push_limited(
        Injection::ParentMessage {
            from: "orchestrator".into(),
            text: "later".into()
        },
        MAX_QUEUED_PARENT_MESSAGES,
        is_parent_message,
    ));
}

#[test]
fn parent_message_is_wrapped_with_an_instruction() {
    let msg = render_injections(
        &[Injection::ParentMessage {
            from: "orchestrator".into(),
            text: "also cover the CLI".into(),
        }],
        1000,
    );
    let text = msg.text();
    assert!(
        text.contains(
            "<message_from_orchestrator>\nalso cover the CLI\n</message_from_orchestrator>"
        )
    );
    assert!(text.contains("Take this into account and continue your task."));
}

#[test]
fn agent_question_names_the_agent_options_and_the_reply_route() {
    let msg = render_injections(
        &[Injection::AgentQuestion {
            agent: info("tests"),
            question_id: "q3".into(),
            question: "Which runner?".into(),
            options: vec!["nextest".into(), "cargo test".into()],
        }],
        1000,
    );
    let text = msg.text();
    assert!(text.contains("<question_from_agent id=\"q3\" agent=\"tests\">"));
    assert!(text.contains("Which runner?"));
    assert!(text.contains("- nextest\n- cargo test\n"));
    assert!(text.contains("</question_from_agent>"));
    assert!(text.contains("action `answer`"));
    assert!(text.contains("agent_id `agt_tests`"));
    assert!(text.contains("question_id `q3`"));
}

#[test]
fn agent_question_without_options_has_no_options_block() {
    let text = render_injections(
        &[Injection::AgentQuestion {
            agent: info("tests"),
            question_id: "q1".into(),
            question: "Why?".into(),
            options: Vec::new(),
        }],
        1000,
    )
    .text();
    assert!(!text.contains("Options:"));
}

#[test]
fn late_answer_is_tagged_with_its_question() {
    let text = render_injections(
        &[Injection::ParentAnswer {
            question_id: "q3".into(),
            text: "nextest".into(),
        }],
        1000,
    )
    .text();
    assert!(text.contains(
        "<answer_from_orchestrator question=\"q3\">\nnextest\n</answer_from_orchestrator>"
    ));
}

#[test]
fn comm_errors_become_tool_failures_with_the_same_text() {
    let err: ToolError = CommError::NotRunning(AgentId::new("agt_x")).into();
    assert!(err.to_string().contains("spawn a new agent"), "{err}");
}
