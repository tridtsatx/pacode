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
    let mut meter_reset = false;
    let mut badge_settled = false;
    while let Ok((_, event)) = events.try_recv() {
        announced |= event == Event::ContextCleared;
        meter_reset |= matches!(
            &event,
            Event::UsageUpdated(usage) if usage.context_tokens == 0 && usage.turns == turns_before
        ) && announced;
        badge_settled |= matches!(event, Event::NoticeLockChanged { held: 0, .. }) && announced;
    }
    assert!(announced, "attached clients reset on ContextCleared");
    assert!(
        meter_reset,
        "every client folds UsageUpdated, so the meter rides it too"
    );
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

fn digger() -> pacode_tools::AgentSpec {
    pacode_tools::AgentSpec {
        prompt: "dig through the logs".into(),
        name: Some("digger".into()),
        model: None,
        effort: None,
        fork: false,
        tools: None,
        exclude_tools: None,
    }
}

/// A subagent that ran to the end — so `interrupt_all` has nothing to stop
/// and a test holding its `message_write_lock` blocks only the clear — and
/// the main turn its report woke.
async fn finished_subagent(
    h: &Harness,
    session: &Arc<crate::session::Session>,
) -> Arc<crate::agent::Agent> {
    let main = session.main_agent().unwrap();
    h.mock.push(MockResponse::Text("found it".into()));
    h.mock.push(MockResponse::Text("noted".into()));
    let id = session.spawn_agent(digger()).await.unwrap();
    let sub = session.agent(&id).unwrap();
    wait_until(|| {
        sub.status() == pacode_types::AgentStatus::Finished
            && h.mock.requests().len() == 2
            && !main.is_running()
    })
    .await;
    sub
}

/// Spawn the clear and return once it is parked on `sub`'s write lock, which
/// the caller holds: the clear then holds every turn lock, `submit_lock` and
/// the main agent's write lock — taken right before the subagents' — and
/// nothing else in these tests takes that last one.
async fn clear_parked_on(
    h: &Harness,
    id: &SessionId,
    session: &Arc<crate::session::Session>,
) -> tokio::task::JoinHandle<Reply> {
    let main = session.main_agent().unwrap();
    let core = Arc::clone(&h.core);
    let id = id.clone();
    let clear = tokio::spawn(async move { core.handle(&id, Request::ClearContext).await });
    wait_until(|| main.message_write_lock.try_lock().is_err()).await;
    clear
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_write_in_flight_lands_before_the_purge_and_goes_with_it() {
    let h = Harness::new().await;
    let (id, session) = open_named(&h, false).await;
    let sub = finished_subagent(&h, &session).await;
    let write = sub.message_write_lock.clone().lock_owned().await;

    let clear = clear_parked_on(&h, &id, &session).await;
    assert!(
        session.agent(&sub.id()).is_some(),
        "the clear may not drop an agent whose row write is in flight"
    );
    // What `stop_agent` or `publish_agent` does under that lock.
    session
        .store
        .upsert_agent(&id, &sub.info(), None)
        .await
        .unwrap();
    drop(write);

    assert_eq!(clear.await.unwrap(), Reply::Ok);
    let stored = session.store.list_agents(&id).await.unwrap();
    assert!(stored.iter().all(|info| info.id.is_main()), "{stored:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_write_cannot_bring_a_cleared_subagent_back() {
    let h = Harness::new().await;
    let (id, session) = open_named(&h, false).await;
    let sub = finished_subagent(&h, &session).await;
    assert_eq!(h.core.handle(&id, Request::ClearContext).await, Reply::Ok);
    let mut events = session.events.subscribe();

    // The write `set_model`/`set_effort` or the stopped-subagent landing would
    // make with an `Arc` taken before the clear.
    session.publish_agent(&sub, sub.info()).await;

    let stored = session.store.list_agents(&id).await.unwrap();
    assert!(stored.iter().all(|info| info.id.is_main()), "{stored:?}");
    while let Ok((_, event)) = events.try_recv() {
        assert!(
            !matches!(&event, Event::AgentUpdated(info) if info.id == sub.id()),
            "no client may get the dropped card back: {event:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_clear_still_wakes_the_main_agent() {
    let h = Harness::new().await;
    let (id, session) = open_named(&h, false).await;
    let main = session.main_agent().unwrap();
    let sub = finished_subagent(&h, &session).await;
    let write = sub.message_write_lock.clone().lock_owned().await;
    let clear = clear_parked_on(&h, &id, &session).await;

    // Close lands while the clear waits, and a notice arrives that finds the
    // main turn lock taken — so nothing but the clear can deliver it.
    session
        .closed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    h.mock.push(MockResponse::Text("woken".into()));
    session.inject_and_wake(
        &main,
        crate::inject::Injection::SystemNotice("wake after the clear".into()),
    );
    drop(write);

    assert!(matches!(clear.await.unwrap(), Reply::Error { .. }));
    wait_until(|| {
        h.mock
            .requests()
            .iter()
            .any(|request| mentions(&request.messages, "wake after the clear"))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_started_after_the_sweep_is_cancelled_not_waited_out() {
    let h = Harness::new().await;
    let (_id, session) = open_named(&h, true).await;
    let main = session.main_agent().unwrap();
    // The gate never opens on its own: this is the turn a redrive or a
    // monitor starts between `interrupt_all` and the lock acquisition.
    session
        .submit_user_message("started in the gap".into())
        .await
        .unwrap();
    h.wait_for_gate().await;

    let agents = session
        .agents
        .read()
        .unwrap()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let guards = tokio::time::timeout(
        Duration::from_secs(5),
        crate::clear::lock_turns_interrupting(&session, &agents),
    )
    .await
    .expect("the lock waits for an unwind, never for a whole turn");

    assert_eq!(guards.len(), agents.len());
    drop(guards);
    wait_until(|| !main.is_running()).await;
}
