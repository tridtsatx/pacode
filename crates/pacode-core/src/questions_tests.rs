use pacode_types::{CallId, QuestionOption, QuestionOrigin};

use super::*;

fn question(id: &str, agent: AgentId) -> Question {
    Question::new(
        QuestionId::new(id),
        QuestionOrigin::new(agent, "main", CallId::new("call_1")),
        "Storage",
        "Where should the cache live?",
        vec![
            QuestionOption::new("In the cache dir", "the usual place").recommended(),
            QuestionOption::new("Next to the project", "portable"),
        ],
        false,
        0,
    )
    .expect("question")
}

#[tokio::test]
async fn answering_a_question_wakes_the_waiting_call() {
    let state = QuestionState::default();
    let q = question("qst_1", AgentId::main());
    let rx = state.register(q.clone()).expect("register");
    assert_eq!(state.pending().len(), 1);

    let resolved = state
        .resolve(&q.id, QuestionAnswer::choice(0))
        .expect("resolve");
    assert_eq!(resolved.id, q.id);
    assert_eq!(rx.await.expect("answer"), QuestionAnswer::choice(0));
    assert!(state.pending().is_empty());
}

#[tokio::test]
async fn answering_an_unknown_question_is_reported_not_ignored_silently() {
    let state = QuestionState::default();
    assert!(
        state
            .resolve(&QuestionId::new("qst_nope"), QuestionAnswer::choice(0))
            .is_none()
    );
}

#[tokio::test]
async fn a_stopped_agent_cancels_its_questions_instead_of_leaving_them_hanging() {
    let state = QuestionState::default();
    let agent = AgentId::new("agt_1");
    let mine = question("qst_1", agent.clone());
    let other = question("qst_2", AgentId::main());
    let rx = state.register(mine).expect("register");
    state.register(other).expect("register");

    let cancelled = state.cancel_for_agent(&agent);
    assert_eq!(cancelled.len(), 1);
    assert!(rx.await.expect("answer").cancelled);
    // The other agent's question is untouched.
    assert_eq!(state.pending().len(), 1);
}

#[tokio::test]
async fn the_pending_cap_refuses_rather_than_growing_without_bound() {
    let state = QuestionState::default();
    let mut receivers = Vec::new();
    for i in 0..MAX_PENDING {
        let rx = state
            .register(question(&format!("qst_{i}"), AgentId::main()))
            .expect("register");
        receivers.push(rx);
    }
    assert!(
        state
            .register(question("qst_over", AgentId::main()))
            .is_none()
    );
    assert_eq!(state.pending().len(), MAX_PENDING);
}

#[tokio::test]
async fn a_question_can_be_looked_up_while_it_waits() {
    let state = QuestionState::default();
    let q = question("qst_1", AgentId::main());
    let _rx = state.register(q.clone()).expect("register");
    assert_eq!(state.get(&q.id).map(|q| q.header), Some("Storage".into()));
    assert!(state.get(&QuestionId::new("qst_nope")).is_none());
}
