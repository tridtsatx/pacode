use pacode_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, Effort, Message, Mode, ModelRoute, Plan, PlanItem,
    PlanStatus, SessionId, SessionMeta,
};

use crate::{MessageAdmissionResult, Store};

fn meta(id: &str) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(id),
        name: Some(id.to_string()),
        cwd: std::path::PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        model: ModelRoute::new("mock", "mock"),
        effort: Effort::Medium,
        mode: Mode::Build,
        first_prompt: None,
        zdr: false,
    }
}

fn agent(id: AgentId, kind: AgentKind) -> AgentInfo {
    AgentInfo {
        id,
        name: "agent".into(),
        kind,
        status: AgentStatus::Idle,
        activity: None,
        started_at_ms: 1,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock"),
        effort: Effort::Medium,
        parent: None,
        summary: None,
        error: None,
    }
}

fn plan() -> Plan {
    Plan {
        version: 3,
        items: vec![PlanItem {
            id: "p1".into(),
            content: "port the parser".into(),
            status: PlanStatus::Active,
            progress: None,
        }],
    }
}

fn texts(rows: &[crate::MessageRow]) -> Vec<String> {
    rows.iter().map(|row| row.message.text()).collect()
}

/// A session with a conversation worth clearing: main messages behind an
/// admission (one of them interrupted), a compaction summary, a plan, and a
/// subagent with history and a summary of its own.
async fn seeded_store() -> (Store, SessionId, AgentId) {
    let store = Store::open_in_memory().expect("store");
    let session = SessionId::new("ses_cleared");
    let main = AgentId::main();
    let sub = AgentId::new("agt_sub");
    store.upsert_session(&meta("ses_cleared")).await.unwrap();
    store
        .upsert_agent(&session, &agent(main.clone(), AgentKind::Main), None)
        .await
        .unwrap();
    store
        .upsert_agent(&session, &agent(sub.clone(), AgentKind::Sub), Some("dig"))
        .await
        .unwrap();
    let admitted = store
        .append_user_message(
            &session,
            &main,
            0,
            "m0",
            7,
            &Message::user("alpha question"),
        )
        .await
        .unwrap();
    assert_eq!(admitted, MessageAdmissionResult::Appended);
    store
        .append_message(&session, &main, 1, &Message::user("alpha answer"))
        .await
        .unwrap();
    store
        .append_user_message(&session, &main, 2, "m2", 9, &Message::user("alpha dropped"))
        .await
        .unwrap();
    store
        .mark_message_admission_terminal(&session, "m2", Some("interrupted"))
        .await
        .unwrap();
    store
        .append_message(&session, &main, 3, &Message::user("alpha tail"))
        .await
        .unwrap();
    store
        .append_message(&session, &sub, 0, &Message::user("alpha sub"))
        .await
        .unwrap();
    store
        .save_compaction(&session, &main, "alpha summary", 1)
        .await
        .unwrap();
    store
        .save_compaction(&session, &sub, "alpha sub summary", 0)
        .await
        .unwrap();
    store.save_plan(&session, &plan()).await.unwrap();
    (store, session, sub)
}

#[tokio::test]
async fn a_cleared_session_loads_nothing_from_before_the_clear() {
    let (store, session, sub) = seeded_store().await;
    let main = AgentId::main();

    store.clear_context(&session, 4).await.unwrap();

    assert!(
        store
            .load_messages(&session, &main)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.max_message_seq(&session, &main).await.unwrap(), None);
    assert!(
        store
            .load_messages(&session, &sub)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.load_compaction(&session, &main).await.unwrap(), None);
    assert_eq!(store.load_compaction(&session, &sub).await.unwrap(), None);
    assert_eq!(store.load_plan(&session).await.unwrap(), Plan::default());
    let agents = store.list_agents(&session).await.unwrap();
    assert_eq!(
        agents.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
        vec![main.clone()],
        "subagents go with the conversation, the main row stays"
    );
    assert!(
        store.search("alpha", 10).await.unwrap().is_empty(),
        "the search index follows the deleted rows"
    );
    assert!(store.get_session(&session).await.unwrap().is_some());
}

#[tokio::test]
async fn a_cleared_session_keeps_admissions_as_idempotency_but_not_their_seqs() {
    let (store, session, _sub) = seeded_store().await;
    let main = AgentId::main();

    store.clear_context(&session, 4).await.unwrap();

    // A replay of a message admitted before the clear is still a duplicate…
    let replay = store
        .append_user_message(
            &session,
            &main,
            0,
            "m0",
            7,
            &Message::user("alpha question"),
        )
        .await
        .unwrap();
    assert_eq!(replay, MessageAdmissionResult::Duplicate);
    let admission = store.get_message_admission(&session, "m2").await.unwrap();
    assert_eq!(admission.and_then(|a| a.message_seq), None);
    // …and the interrupted one no longer hides the message that reuses its seq
    // once a resume restarts the counter from an empty table.
    assert!(
        store
            .cancelled_message_seqs(&session, &main)
            .await
            .unwrap()
            .is_empty()
    );
    store
        .append_message(&session, &main, 2, &Message::user("fresh start"))
        .await
        .unwrap();
    let page = store
        .load_messages_after(&session, &main, None, 10)
        .await
        .unwrap();
    assert_eq!(texts(&page.rows), vec!["fresh start".to_string()]);
}

#[tokio::test]
async fn a_clear_keeps_main_rows_past_the_boundary_and_other_sessions() {
    let (store, session, _sub) = seeded_store().await;
    let main = AgentId::main();
    let other = SessionId::new("ses_other");
    store.upsert_session(&meta("ses_other")).await.unwrap();
    store
        .append_message(&other, &main, 0, &Message::user("alpha elsewhere"))
        .await
        .unwrap();
    store.save_plan(&other, &plan()).await.unwrap();
    store
        .append_message(&session, &main, 4, &Message::user("after the boundary"))
        .await
        .unwrap();

    store.clear_context(&session, 4).await.unwrap();

    let kept = store.load_messages(&session, &main).await.unwrap();
    assert_eq!(texts(&kept), vec!["after the boundary".to_string()]);
    let elsewhere = store.load_messages(&other, &main).await.unwrap();
    assert_eq!(texts(&elsewhere), vec!["alpha elsewhere".to_string()]);
    assert_eq!(store.load_plan(&other).await.unwrap(), plan());
    let hits = store.search("alpha", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session, other);
}
