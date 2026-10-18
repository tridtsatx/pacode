use std::sync::Arc;
use std::time::Duration;

use pacode_config::Paths;
use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_provider::mock::MockProvider;
use pacode_store::Store;
use pacode_tools::builtin::ask_parent::{self, ParentReply};
use pacode_tools::builtin::builtin_tools;
use pacode_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, Attach, Config, Effort, ModelRoute, WebConfig,
};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, History};
use crate::agent_comm::{CommError, MAX_QUEUED_PARENT_MESSAGES};
use crate::core::{Core, CoreDeps};
use crate::inject::Injection;
use crate::session::Session;

async fn setup(timeout_secs: u64, max_questions: u32) -> (Arc<Session>, tempfile::TempDir) {
    let temp_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let exec_cfg = pacode_types::ExecConfig {
        yield_after_secs: 1,
        stall_secs: 100,
        max_spool_bytes: 50 * 1024 * 1024,
        tail_bytes: 64 * 1024,
        kill_grace_secs: 5,
        max_tasks: 64,
        default_timeout_secs: 60,
    };
    let tasks = TaskManager::new(temp_dir.path().to_path_buf(), exec_cfg);
    let mcp = McpPool::new(Default::default(), None, None);

    let mock = Arc::new(MockProvider::new("mock"));
    let mut reg = ProviderRegistry::empty();
    reg.insert(mock);
    reg.set_default_route(Some(ModelRoute::new("mock", "mock-model")));

    let mut config = Config::default();
    config.provider.default = Some("mock/mock-model".to_string());
    config.agents.ask_parent_timeout_secs = timeout_secs;
    config.agents.ask_parent_max = max_questions;

    let paths = Paths::under(temp_dir.path());
    let deps = CoreDeps {
        marketplace: Arc::new(pacode_plugin::marketplace::Marketplace::new(
            Arc::new(pacode_plugin::marketplace::HttpFetcher::new()),
            paths.cache_dir.join("marketplace"),
            paths.cache_dir.join("plugins"),
            pacode_plugin::marketplace::cache::DEFAULT_TTL_SECS,
        )),
        config: Arc::new(config),
        paths,
        providers: Arc::new(reg),
        tools: builtin_tools(&WebConfig::default()),
        tasks,
        mcp,
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        store,
        app_version: "0.1.0-test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
    };
    let core = Core::new(deps).await;
    let session_id = core
        .open_session(Attach::New {
            cwd: temp_dir.path().to_path_buf(),
            model: Some(ModelRoute::new("mock", "mock-model")),
            effort: None,
            mode: None,
        })
        .await
        .unwrap();
    let session = core.session(&session_id).unwrap();
    // The main agent counts as mid-turn, so a question does not start a real turn.
    let main = session.main_agent().unwrap();
    *main.cancel.lock().unwrap() = Some(CancellationToken::new());
    (session, temp_dir)
}

/// A subagent registered by hand; `running` gives it a turn token like a real one.
fn add_sub(session: &Session, id: &str, name: &str, running: bool) -> Arc<Agent> {
    let id = AgentId::new(id);
    let info = AgentInfo {
        id: id.clone(),
        name: name.to_string(),
        kind: AgentKind::Sub,
        status: if running {
            AgentStatus::Thinking
        } else {
            AgentStatus::Finished
        },
        activity: Some("thinking".into()),
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "mock-model"),
        effort: Effort::Low,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    let agent = Arc::new(Agent::new(
        id.clone(),
        info,
        History::default(),
        pacode_tools::ToolRegistry::new(),
        50,
        None,
    ));
    if running {
        *agent.cancel.lock().unwrap() = Some(CancellationToken::new());
    }
    session.agents.write().unwrap().insert(id, agent.clone());
    agent
}

async fn pending_id(agent: &Agent) -> String {
    for _ in 0..300 {
        if let Some(id) = agent.comm.pending_id() {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no question became pending");
}

fn ask(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    wait: bool,
) -> tokio::task::JoinHandle<Result<ParentReply, CommError>> {
    let session = session.clone();
    let id = agent.id();
    tokio::spawn(async move {
        session
            .ask_parent(
                &id,
                "Which runner?".into(),
                vec!["a".into(), "b".into()],
                wait,
            )
            .await
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn message_to_a_finished_agent_tells_the_orchestrator_to_spawn_a_new_one() {
    let (session, _tmp) = setup(600, 5).await;
    let done = add_sub(&session, "agt_done", "done", false);
    let err = session.message_agent(&done.id(), "hello").unwrap_err();
    assert_eq!(err, CommError::NotRunning(done.id()));
    assert!(err.to_string().contains("spawn a new agent"));
    assert!(done.injections.is_empty());

    let missing = session.message_agent(&AgentId::new("agt_nope"), "hello");
    assert_eq!(missing, Err(CommError::NotFound(AgentId::new("agt_nope"))));
    assert_eq!(
        session.message_agent(&AgentId::main(), "hello"),
        Err(CommError::IsMain)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn message_is_queued_for_the_next_step_and_capped() {
    let (session, _tmp) = setup(600, 5).await;
    let sub = add_sub(&session, "agt_run", "run", true);

    for i in 0..MAX_QUEUED_PARENT_MESSAGES {
        session
            .message_agent(&sub.id(), &format!("  note {i}  "))
            .unwrap();
    }
    let err = session
        .message_agent(&sub.id(), "one too many")
        .unwrap_err();
    assert_eq!(
        err,
        CommError::QueueFull {
            agent: sub.id(),
            cap: MAX_QUEUED_PARENT_MESSAGES
        }
    );

    let drained = sub.injections.drain();
    assert_eq!(drained.len(), MAX_QUEUED_PARENT_MESSAGES);
    assert_eq!(
        drained[0],
        Injection::ParentMessage {
            from: "orchestrator".into(),
            text: "note 0".into()
        }
    );
    // The agent shows the message in its transcript.
    let (items, _) = sub.transcript_tail(None, 10);
    assert!(items.iter().any(|i| matches!(
        &i.kind,
        pacode_types::TranscriptKind::Notice { text, .. } if text.starts_with("message ←")
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_or_oversized_messages_are_refused() {
    let (session, _tmp) = setup(600, 5).await;
    let sub = add_sub(&session, "agt_run", "run", true);
    assert_eq!(
        session.message_agent(&sub.id(), "   "),
        Err(CommError::EmptyText)
    );
    let long = "x".repeat(4001);
    assert!(matches!(
        session.message_agent(&sub.id(), &long),
        Err(CommError::TextTooLong { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_main_agent_cannot_ask_and_has_no_ask_parent_tool() {
    let (session, _tmp) = setup(600, 5).await;
    let err = session
        .ask_parent(&AgentId::main(), "q".into(), Vec::new(), true)
        .await
        .unwrap_err();
    assert_eq!(err, CommError::NoParent);

    let main = session.main_agent().unwrap();
    assert!(main.tools.read().unwrap().get(ask_parent::NAME).is_none());
    assert!(main.tools.read().unwrap().get("agent").is_some());
    // The session set still has it, so subagents get it.
    let all = session.tools.read().unwrap();
    assert!(all.get(ask_parent::NAME).is_some());
    let sub_names = pacode_tools::subagent_tool_names(&all);
    assert!(sub_names.iter().any(|n| n == ask_parent::NAME));
}

#[tokio::test(flavor = "multi_thread")]
async fn non_blocking_question_reaches_the_orchestrator_and_the_answer_comes_back() {
    let (session, _tmp) = setup(600, 5).await;
    let main = session.main_agent().unwrap();
    let sub = add_sub(&session, "agt_tests", "tests", true);

    let reply = ask(&session, &sub, false).await.unwrap().unwrap();
    let ParentReply::Pending { question_id } = reply else {
        panic!("expected Pending, got {reply:?}");
    };

    let injected = main.injections.drain();
    assert_eq!(injected.len(), 1);
    match &injected[0] {
        Injection::AgentQuestion {
            agent,
            question_id: id,
            question,
            options,
        } => {
            assert_eq!(agent.name, "tests");
            assert_eq!(id, &question_id);
            assert_eq!(question, "Which runner?");
            assert_eq!(options, &vec!["a".to_string(), "b".to_string()]);
        }
        other => panic!("unexpected injection {other:?}"),
    }
    // The asking agent keeps running: it is not marked as waiting.
    assert_eq!(sub.status(), AgentStatus::Thinking);

    // A second question waits for the first.
    let again = ask(&session, &sub, false).await.unwrap().unwrap_err();
    assert_eq!(again, CommError::QuestionPending(question_id.clone()));

    // Wrong id is rejected, the right one is injected for the subagent.
    assert!(matches!(
        session.answer_agent(&sub.id(), "q0", "x"),
        Err(CommError::WrongQuestion { .. })
    ));
    session.answer_agent(&sub.id(), &question_id, "a").unwrap();
    assert_eq!(
        sub.injections.drain(),
        vec![Injection::ParentAnswer {
            question_id,
            text: "a".into()
        }]
    );
    assert_eq!(sub.comm.pending_id(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_question_waits_for_the_answer_and_restores_the_status() {
    let (session, _tmp) = setup(600, 5).await;
    let sub = add_sub(&session, "agt_tests", "tests", true);

    let handle = ask(&session, &sub, true);
    let qid = pending_id(&sub).await;
    assert_eq!(sub.status(), AgentStatus::WaitingOrchestrator);
    assert_eq!(
        sub.info().activity.as_deref(),
        Some("waiting for orchestrator")
    );

    session.answer_agent(&sub.id(), &qid, "use a").unwrap();
    let reply = handle.await.unwrap().unwrap();
    assert_eq!(
        reply,
        ParentReply::Answered {
            question_id: qid,
            text: "use a".into()
        }
    );
    assert_eq!(sub.status(), AgentStatus::Thinking);
    assert_eq!(sub.info().activity.as_deref(), Some("thinking"));
    // The answer went to the blocked call, not into the queue.
    assert!(sub.injections.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plain_message_resolves_a_blocked_question() {
    let (session, _tmp) = setup(600, 5).await;
    let sub = add_sub(&session, "agt_tests", "tests", true);
    let handle = ask(&session, &sub, true);
    let qid = pending_id(&sub).await;

    session.message_agent(&sub.id(), "just use a").unwrap();
    assert_eq!(
        handle.await.unwrap().unwrap(),
        ParentReply::Answered {
            question_id: qid,
            text: "just use a".into()
        }
    );
    assert!(sub.injections.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_question_times_out_and_the_orchestrator_is_told() {
    let (session, _tmp) = setup(1, 5).await;
    let main = session.main_agent().unwrap();
    let sub = add_sub(&session, "agt_tests", "tests", true);

    let reply = ask(&session, &sub, true).await.unwrap().unwrap();
    let ParentReply::TimedOut { question_id } = reply else {
        panic!("expected TimedOut, got {reply:?}");
    };
    assert_eq!(
        ask_parent::reply_text(&ParentReply::TimedOut {
            question_id: question_id.clone()
        }),
        "no answer from orchestrator; make a reasonable assumption and state it"
    );
    assert_eq!(sub.comm.pending_id(), None);
    assert_eq!(sub.status(), AgentStatus::Thinking);

    let injected = main.injections.drain();
    assert!(
        injected
            .iter()
            .any(|i| matches!(i, Injection::AgentQuestion { .. }))
    );
    assert!(injected.iter().any(|i| matches!(
        i,
        Injection::SystemNotice(t) if t.contains(&question_id) && t.contains("expired")
    )));
    // Too late to answer now.
    assert!(matches!(
        session.answer_agent(&sub.id(), &question_id, "late"),
        Err(CommError::NoPendingQuestion(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_agent_cancels_its_blocked_question() {
    let (session, _tmp) = setup(600, 5).await;
    let sub = add_sub(&session, "agt_tests", "tests", true);
    let handle = ask(&session, &sub, true);
    let qid = pending_id(&sub).await;

    session.stop_agent(&sub.id()).await.unwrap();
    assert_eq!(
        handle.await.unwrap().unwrap(),
        ParentReply::Cancelled { question_id: qid }
    );
    assert_eq!(sub.comm.pending_id(), None);
    // The stop stays: the waiter does not overwrite it with its old status.
    assert_eq!(sub.status(), AgentStatus::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_turn_token_cancelling_wakes_a_blocked_question() {
    let (session, _tmp) = setup(600, 5).await;
    let sub = add_sub(&session, "agt_tests", "tests", true);
    let token = sub.cancel.lock().unwrap().clone().unwrap();
    let handle = ask(&session, &sub, true);
    let qid = pending_id(&sub).await;

    token.cancel();
    assert_eq!(
        handle.await.unwrap().unwrap(),
        ParentReply::Cancelled { question_id: qid }
    );
    assert_eq!(sub.comm.pending_id(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_that_keeps_asking_hits_the_limit() {
    let (session, _tmp) = setup(600, 2).await;
    let sub = add_sub(&session, "agt_tests", "tests", true);
    for _ in 0..2 {
        let reply = ask(&session, &sub, false).await.unwrap().unwrap();
        let ParentReply::Pending { question_id } = reply else {
            panic!("expected Pending");
        };
        session.answer_agent(&sub.id(), &question_id, "ok").unwrap();
    }
    let err = ask(&session, &sub, false).await.unwrap().unwrap_err();
    assert_eq!(err, CommError::AskLimit(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_agents_open_question_is_dropped() {
    let (session, _tmp) = setup(600, 5).await;
    let sub = add_sub(&session, "agt_tests", "tests", true);
    let reply = ask(&session, &sub, false).await.unwrap().unwrap();
    assert!(matches!(reply, ParentReply::Pending { .. }));
    session.drop_agent_questions(&sub.id());
    assert_eq!(sub.comm.pending_id(), None);
}
