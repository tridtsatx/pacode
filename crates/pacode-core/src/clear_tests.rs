//! `Request::ClearContext`, driven through `Core::handle` on the harness of
//! `core_tests` (declared there, so it shares `Harness` and its helpers).

use super::*;
use pacode_types::{Event, Message, Plan, PlanItem, PlanStatus};

/// A named session: the title request fires only while the name is unset,
/// and it would take scripted mock answers meant for the turns under test.
async fn open_named(h: &Harness, gated: bool) -> (SessionId, Arc<crate::session::Session>) {
    let id = if gated {
        h.open_gated().await
    } else {
        h.open().await
    };
    let session = h.core.session(&id).unwrap();
    session.meta.write().unwrap().name = Some("clear test".into());
    (id, session)
}

/// Run one main turn to the end: `answer` is what the mock replies.
async fn run_turn(h: &Harness, session: &Arc<crate::session::Session>, text: &str, answer: &str) {
    let main = session.main_agent().unwrap();
    let before = h.mock.requests().len();
    h.mock.push(MockResponse::Text(answer.into()));
    session.submit_user_message(text.into()).await.unwrap();
    wait_until(|| h.mock.requests().len() > before && !main.is_running()).await;
}

fn history_texts(session: &crate::session::Session) -> Vec<String> {
    session
        .main_agent()
        .unwrap()
        .history
        .lock()
        .unwrap()
        .messages
        .iter()
        .map(|message| message.text())
        .collect()
}

fn mentions(messages: &[Arc<Message>], needle: &str) -> bool {
    messages
        .iter()
        .any(|message| message.text().contains(needle))
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_clear_the_provider_sees_no_prior_messages() {
    let h = Harness::new().await;
    let (id, session) = open_named(&h, false).await;
    run_turn(&h, &session, "first question", "first answer").await;
    assert!(
        history_texts(&session)
            .iter()
            .any(|t| t == "first question")
    );

    assert_eq!(h.core.handle(&id, Request::ClearContext).await, Reply::Ok);
    assert!(history_texts(&session).is_empty());
    run_turn(&h, &session, "second question", "second answer").await;

    let request = h
        .mock
        .requests()
        .into_iter()
        .rev()
        .find(|request| mentions(&request.messages, "second question"))
        .expect("the turn after the clear reached the provider");
    assert!(
        !mentions(&request.messages, "first"),
        "nothing from before the clear may reach the model: {:?}",
        request
            .messages
            .iter()
            .map(|message| message.text())
            .collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clear_resets_the_session_state_and_announces_it() {
    let h = Harness::new().await;
    let (id, session) = open_named(&h, false).await;
    run_turn(&h, &session, "first question", "first answer").await;
    session.set_plan(Plan {
        version: 2,
        items: vec![PlanItem {
            id: "p1".into(),
            content: "port the parser".into(),
            status: PlanStatus::Active,
            progress: None,
        }],
    });
    session.usage.write().unwrap().context_tokens = 4_000;
    let turns_before = session.usage.read().unwrap().turns;
    let mut events = session.events.subscribe();

    assert_eq!(h.core.handle(&id, Request::ClearContext).await, Reply::Ok);

    let snapshot = session.snapshot();
    assert!(snapshot.transcript.is_empty());
    assert!(!snapshot.has_more_history);
    assert!(snapshot.plan.items.is_empty());
    assert_eq!(snapshot.usage.context_tokens, 0);
    assert_eq!(
        snapshot.usage.turns, turns_before,
        "what the session spent is not context"
    );
    assert_eq!(snapshot.meta.id, id);
    assert_eq!(snapshot.meta.name.as_deref(), Some("clear test"));
    let mut announced = false;
    let mut badge_settled = false;
    while let Ok((_, event)) = events.try_recv() {
        announced |= event == Event::ContextCleared;
        badge_settled |= matches!(event, Event::NoticeLockChanged { held: 0, .. }) && announced;
    }
    assert!(announced, "attached clients reset on ContextCleared");
    assert!(badge_settled, "the notice badge learns its backlog is gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_session_reloads_nothing_from_before_the_clear() {
    let h = Harness::new().await;
    let (id, session) = open_named(&h, false).await;
    let lease = h.attach(&id).await;
    run_turn(&h, &session, "first question", "first answer").await;
    assert_eq!(h.core.handle(&id, Request::ClearContext).await, Reply::Ok);
    h.core.release_lease(&lease);
    drop(session);
    assert_unloads_when_idle(&h.core, &id).await;

    let (resumed, lease) = h
        .core
        .open_attached(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(resumed, id);
    let reloaded = h.core.session(&id).unwrap();
    assert!(history_texts(&reloaded).is_empty());
    let snapshot = reloaded.snapshot();
    assert!(snapshot.transcript.is_empty());
    assert!(!snapshot.has_more_history);
    assert!(snapshot.plan.items.is_empty());
    assert!(snapshot.agents.iter().all(|agent| agent.id.is_main()));
    h.core.release_lease(&lease);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clear_waits_out_a_running_turn_instead_of_clearing_under_it() {
    let h = Harness::new().await;
    let (id, session) = open_named(&h, true).await;
    let main = session.main_agent().unwrap();
    session
        .submit_user_message("first question".into())
        .await
        .unwrap();
    h.wait_for_gate().await;
    assert!(main.is_running());

    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        h.core.handle(&id, Request::ClearContext),
    )
    .await
    .expect("the clear interrupts the turn rather than waiting for it forever");

    assert_eq!(reply, Reply::Ok);
    assert!(!main.is_running());
    assert!(history_texts(&session).is_empty());
    assert!(session.snapshot().transcript.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clear_stops_and_drops_every_subagent() {
    let h = Harness::new().await;
    let (id, session) = open_named(&h, false).await;
    h.mock.push(MockResponse::Slow {
        text: "still digging".into(),
        delay: Duration::from_secs(30),
    });
    let sub = session
        .spawn_agent(pacode_tools::AgentSpec {
            prompt: "dig through the logs".into(),
            name: Some("digger".into()),
            model: None,
            effort: None,
            fork: false,
            tools: None,
            exclude_tools: None,
        })
        .await
        .unwrap();
    let sub_agent = session.agent(&sub).unwrap();
    wait_until(|| sub_agent.is_running()).await;

    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        h.core.handle(&id, Request::ClearContext),
    )
    .await
    .expect("the clear stops a running subagent");

    assert_eq!(reply, Reply::Ok);
    assert!(!sub_agent.is_running());
    assert!(session.agent(&sub).is_none());
    assert!(session.agent_infos().iter().all(|info| info.id.is_main()));
    let stored = session.store.list_agents(&id).await.unwrap();
    assert!(stored.iter().all(|info| info.id.is_main()));
    assert!(
        session
            .store
            .load_messages(&id, &sub)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_clear_leaves_the_conversation_alone() {
    let h = Harness::new().await;
    let (id, session) = open_named(&h, false).await;
    seed_history(&session.main_agent().unwrap(), 2);
    let before = history_texts(&session);
    let mut events = session.events.subscribe();

    session.store.set_fault_injection(true);
    let reply = h.core.handle(&id, Request::ClearContext).await;
    session.store.set_fault_injection(false);

    assert!(
        matches!(reply, Reply::Error { .. }),
        "a store that cannot forget refuses the clear: {reply:?}"
    );
    assert_eq!(history_texts(&session), before);
    while let Ok((_, event)) = events.try_recv() {
        assert_ne!(event, Event::ContextCleared);
    }
}
