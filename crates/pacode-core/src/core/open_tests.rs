use super::{commit_resume_statuses, hydrate_agent_history, insert_if_absent};
use pacode_types::{SessionId, SessionMeta};
use std::collections::BTreeMap;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

/// An agent resumed with a prompt but no persisted message row (a legacy
/// row, a cleared history) still has to hand the model prompt + answer. The
/// prompt takes the seq the next message would, and the cursor moves past
/// it — parking both on seq 0 made the first answer overwrite the prompt.
#[tokio::test]
async fn prompt_fallback_advances_the_message_cursor_past_itself() {
    use pacode_types::{AgentId, ContentBlock, Message, Role};

    let store = pacode_store::Store::open_in_memory().unwrap();
    let session_id = SessionId::new("resume-prompt-cursor");
    let agent_id = AgentId::new("agt_prompt_cursor");
    let mut hydrated = hydrate_agent_history(
        &store,
        &session_id,
        &agent_id,
        None,
        128,
        Some("subagent prompt".into()),
    )
    .await
    .unwrap();

    assert_eq!(hydrated.rows.len(), 1);
    assert_eq!(hydrated.rows[0].0, 0);
    assert_eq!(hydrated.history.message_seqs, vec![0]);
    assert_eq!(hydrated.history.next_seq, 1);

    let (seq, _) = hydrated.history.push(Message::new(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "answer".into(),
        }],
    ));
    assert_eq!(seq, 1);
    assert_eq!(hydrated.history.message_seqs, vec![0, 1]);
    assert_eq!(hydrated.history.messages.len(), 2);
    assert_eq!(hydrated.history.messages[0].text(), "subagent prompt");
    assert_eq!(hydrated.history.messages[1].text(), "answer");
}

#[tokio::test]
async fn prompt_fallback_continues_after_a_summarized_history() {
    use pacode_types::{AgentId, Message, SessionMeta};
    use std::path::PathBuf;

    let store = pacode_store::Store::open_in_memory().unwrap();
    let session_id = SessionId::new("resume-prompt-cursor-compacted");
    store
        .upsert_session(&SessionMeta {
            id: session_id.clone(),
            name: None,
            cwd: PathBuf::from("."),
            git_branch: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            model: pacode_types::ModelRoute::new("mock", "mock-model"),
            effort: pacode_types::Effort::Low,
            mode: pacode_types::Mode::Build,
            first_prompt: None,
            zdr: false,
        })
        .await
        .unwrap();
    let agent_id = AgentId::new("agt_prompt_cursor_compacted");
    let mut hydrated = hydrate_agent_history(
        &store,
        &session_id,
        &agent_id,
        Some(("summary".to_string(), 4)),
        128,
        Some("subagent prompt".into()),
    )
    .await
    .unwrap();

    assert_eq!(hydrated.history.message_seqs, vec![5]);
    assert_eq!(hydrated.history.next_seq, 6);
    let (seq, _) = hydrated.history.push(Message::user("follow-up"));
    assert_eq!(seq, 6);
}

#[test]
fn resume_insert_preserves_winner_arc() {
    let id = SessionId::new("resume-insert-race");
    let first = Arc::new(1);
    let second = Arc::new(2);
    let sessions = Arc::new(Mutex::new(BTreeMap::new()));
    let barrier = Arc::new(Barrier::new(2));
    let first_id = id.clone();
    let second_id = id.clone();

    let handles = vec![
        thread::spawn({
            let sessions = sessions.clone();
            let barrier = barrier.clone();
            move || {
                barrier.wait();
                let mut sessions = sessions.lock().unwrap();
                insert_if_absent(&mut sessions, first_id, first)
            }
        }),
        thread::spawn({
            let sessions = sessions.clone();
            let barrier = barrier.clone();
            move || {
                barrier.wait();
                let mut sessions = sessions.lock().unwrap();
                insert_if_absent(&mut sessions, second_id, second)
            }
        }),
    ];
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|(_, inserted)| *inserted).count(), 1);
    assert_eq!(results.iter().filter(|(_, inserted)| !*inserted).count(), 1);
    let winner = results
        .iter()
        .find_map(|(session, inserted)| if *inserted { Some(session) } else { None })
        .unwrap();
    let loser = results
        .iter()
        .find_map(|(session, inserted)| if *inserted { None } else { Some(session) })
        .unwrap();

    assert!(Arc::ptr_eq(winner, loser));
    let stored = sessions.lock().unwrap().get(&id).unwrap().clone();
    assert!(Arc::ptr_eq(&stored, winner));
}

#[tokio::test]
async fn resume_status_commit_is_winner_only() {
    use pacode_types::{
        AgentId, AgentInfo, AgentKind, AgentStatus, Effort, ModelRoute, TaskId, TaskInfo,
        TaskStatus,
    };
    use std::path::PathBuf;

    let session_id = SessionId::new("resume-status-commit");
    let store = pacode_store::Store::open_in_memory().unwrap();
    store
        .upsert_session(&SessionMeta {
            id: session_id.clone(),
            name: None,
            cwd: PathBuf::from("."),
            git_branch: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            model: ModelRoute::new("mock", "mock-model"),
            effort: Effort::Low,
            mode: pacode_types::Mode::Build,
            first_prompt: None,
            zdr: false,
        })
        .await
        .unwrap();

    let task = TaskInfo {
        id: TaskId::generate(),
        session: session_id.clone(),
        owner: AgentId::main(),
        label: "task".into(),
        command: "task".into(),
        cwd: PathBuf::from("."),
        status: TaskStatus::Running,
        backgrounded: false,
        exit_code: None,
        started_at_ms: 1,
        ended_at_ms: None,
        progress: None,
        warnings: 0,
        errors: 0,
        output_path: PathBuf::from("task.log"),
        output_bytes: 0,
        acked: false,
    };
    let subagent = AgentInfo {
        id: AgentId::new("resume-status-subagent"),
        name: "worker".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::Thinking,
        activity: Some("working".into()),
        started_at_ms: 1,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: Effort::Low,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    store.upsert_task(&task).await.unwrap();
    store
        .upsert_agent(&session_id, &subagent, None)
        .await
        .unwrap();

    let mut killed = task.clone();
    killed.status = TaskStatus::Killed;
    let mut finished = subagent.clone();
    finished.status = AgentStatus::Finished;
    assert!(
        !commit_resume_statuses(
            &store,
            &session_id,
            false,
            vec![killed.clone()],
            vec![finished.clone()],
        )
        .await
    );
    assert_eq!(
        store.list_tasks(&session_id).await.unwrap()[0].status,
        TaskStatus::Running
    );
    assert_eq!(
        store.list_agents(&session_id).await.unwrap()[0].status,
        AgentStatus::Thinking
    );

    assert!(commit_resume_statuses(&store, &session_id, true, vec![killed], vec![finished]).await);
    assert_eq!(
        store.list_tasks(&session_id).await.unwrap()[0].status,
        TaskStatus::Killed
    );
    assert_eq!(
        store.list_agents(&session_id).await.unwrap()[0].status,
        AgentStatus::Finished
    );
}

/// A window that does not reach the start of a long history still opens
/// on the task the agent was given, and never on the results of a call
/// whose request fell outside it.
#[tokio::test]
async fn a_truncated_window_keeps_the_brief_and_starts_on_a_valid_boundary() {
    use pacode_types::{AgentId, CallId, Message, Role};
    use std::path::PathBuf;

    let store = pacode_store::Store::open_in_memory().unwrap();
    let session_id = SessionId::new("resume-truncated-window");
    store
        .upsert_session(&SessionMeta {
            id: session_id.clone(),
            name: None,
            cwd: PathBuf::from("."),
            git_branch: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            model: pacode_types::ModelRoute::new("mock", "mock-model"),
            effort: pacode_types::Effort::Low,
            mode: pacode_types::Mode::Build,
            first_prompt: None,
            zdr: false,
        })
        .await
        .unwrap();
    let agent_id = AgentId::new("agt_truncated_window");
    let call = |id: &str| {
        Message::new(
            Role::Assistant,
            vec![pacode_types::ContentBlock::ToolUse {
                id: CallId::new(id),
                name: "read".into(),
                input: serde_json::json!({"path": "x"}),
            }],
        )
    };
    let history = [
        Message::user("the brief"),
        call("a"),
        Message::tool_result(CallId::new("a"), "result a", false),
        call("b"),
        Message::tool_result(CallId::new("b"), "result b", false),
        Message::assistant_text("progress"),
        Message::user("more"),
        call("c"),
        Message::tool_result(CallId::new("c"), "result c", false),
        Message::assistant_text("done"),
    ];
    for (seq, message) in history.iter().enumerate() {
        store
            .append_message(&session_id, &agent_id, seq as u64, message)
            .await
            .unwrap();
    }

    // Six rows back from the end lands on the result of `b`.
    let hydrated = hydrate_agent_history(
        &store,
        &session_id,
        &agent_id,
        None,
        6,
        Some("the brief".into()),
    )
    .await
    .unwrap();
    assert_eq!(hydrated.history.message_seqs, vec![5, 6, 7, 8, 9]);
    assert_ne!(hydrated.history.messages[0].role, Role::Tool);
    let shown = hydrated.history.for_model();
    assert!(shown[0].text().contains("the brief"), "{shown:#?}");

    // With a compaction summary, the brief leads it.
    let compacted = hydrate_agent_history(
        &store,
        &session_id,
        &agent_id,
        Some(("summary of 0..=4".into(), 4)),
        6,
        Some("the brief".into()),
    )
    .await
    .unwrap();
    let summary = compacted.history.summary.unwrap();
    assert!(
        summary.starts_with("Your original task:\nthe brief"),
        "{summary}"
    );
    assert!(summary.contains("summary of 0..=4"));

    // A window that reaches the start changes nothing.
    let whole = hydrate_agent_history(
        &store,
        &session_id,
        &agent_id,
        None,
        128,
        Some("the brief".into()),
    )
    .await
    .unwrap();
    assert_eq!(whole.history.message_seqs.len(), 10);
    assert!(whole.history.summary.is_none());
}
