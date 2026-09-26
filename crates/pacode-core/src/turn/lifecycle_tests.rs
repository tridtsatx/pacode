use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pacode_provider::{CompletionRequest, EventStream, Provider, ProviderError};
use pacode_tools::ToolRegistry;
use pacode_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, Config, Event, Message, ModelInfo, ModelRoute,
    Role, SessionId, SessionMeta, StopReason, StreamEvent, TurnId, TurnStop, Usage,
};
use tokio::sync::Barrier;
use tokio::time::timeout;

use super::{
    install_lifecycle_gate, park_admission_for_recovery, persisted_terminal_stop, request_turn,
    restore_admission_message, start_turn, terminal_stop,
};
use crate::agent::{Agent, History};
use crate::inject::Injection;
use crate::session::Session;
use crate::turn::{compaction_input_tokens, hide_other_admission_messages};

static SESSION_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
static LIFECYCLE_GATE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct BlockingProvider {
    block_first: bool,
    calls: AtomicUsize,
    requests: Mutex<Vec<CompletionRequest>>,
    entered: Barrier,
    release: Barrier,
}

impl BlockingProvider {
    fn new(block_first: bool) -> Arc<Self> {
        Arc::new(Self {
            block_first,
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            entered: Barrier::new(2),
            release: Barrier::new(2),
        })
    }

    fn requests(&self) -> Vec<CompletionRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[async_trait]
impl Provider for BlockingProvider {
    fn id(&self) -> &str {
        "barrier"
    }

    async fn complete(&self, request: CompletionRequest) -> Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request);
        if self.block_first && call == 0 {
            barrier_wait(&self.entered).await;
            barrier_wait(&self.release).await;
        }
        let events = vec![
            Ok(StreamEvent::TextDelta {
                text: format!("answer-{call}"),
            }),
            Ok(StreamEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 2,
                ..Usage::default()
            })),
            Ok(StreamEvent::MessageEnd {
                stop: StopReason::EndTurn,
            }),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![self.model_info("mock-model")])
    }

    fn model_info(&self, _model: &str) -> ModelInfo {
        ModelInfo {
            route: ModelRoute::new("barrier", "mock-model"),
            display_name: "Barrier".into(),
            context_window: Some(128_000),
            supports_reasoning: false,
            supports_vision: false,
            supported_efforts: None,
            pricing: None,
        }
    }
}

async fn test_session(
    provider: Arc<BlockingProvider>,
) -> (tempfile::TempDir, Arc<Session>, Arc<Agent>, SessionId) {
    let temp = tempfile::tempdir().unwrap();
    let sequence = SESSION_SEQUENCE.fetch_add(1, Ordering::SeqCst);
    let session_id = pacode_types::SessionId::new(format!("turn-lifecycle-tests-{sequence}"));
    let agent_id = AgentId::main();
    let now = pacode_types::now_ms();
    let route = ModelRoute::new("barrier", "mock-model");
    let meta = SessionMeta {
        id: session_id.clone(),
        name: Some("turn lifecycle tests".into()),
        cwd: temp.path().to_path_buf(),
        git_branch: None,
        created_at_ms: now,
        updated_at_ms: now,
        model: route.clone(),
        effort: pacode_types::Effort::Medium,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
        zdr: false,
    };
    let info = AgentInfo {
        id: agent_id.clone(),
        name: "main".into(),
        kind: AgentKind::Main,
        status: AgentStatus::Idle,
        activity: None,
        started_at_ms: now,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: route,
        effort: pacode_types::Effort::Medium,
        parent: None,
        summary: None,
        error: None,
    };
    let tools = ToolRegistry::new();
    let agent = Arc::new(Agent::new(
        agent_id.clone(),
        info,
        History::default(),
        tools.clone(),
        200,
        None,
    ));
    let mut agents = BTreeMap::new();
    agents.insert(agent_id, agent.clone());
    let mut providers = pacode_provider::ProviderRegistry::empty();
    providers.insert(provider);
    providers.set_default_route(Some(ModelRoute::new("barrier", "mock-model")));
    let mut config = Config::default();
    config.provider.default = Some("barrier/mock-model".into());
    let store = pacode_store::Store::open_in_memory().unwrap();
    store.upsert_session(&meta).await.unwrap();
    let session = Arc::new(Session {
        id: session_id.clone(),
        meta: std::sync::RwLock::new(meta),
        agents: std::sync::RwLock::new(agents),
        agent_aliases: std::sync::Mutex::new(HashMap::new()),
        plan: std::sync::RwLock::new(Default::default()),
        usage: std::sync::RwLock::new(Default::default()),
        permissions: crate::permissions::PermissionState::default(),
        questions: crate::questions::QuestionState::default(),
        events: crate::transcript::EventSink::new(),
        tasks: pacode_exec::TaskManager::new(
            temp.path().join("spool"),
            pacode_types::ExecConfig::default(),
        ),
        store,
        submit_lock: tokio::sync::Mutex::new(()),
        closed: std::sync::atomic::AtomicBool::new(false),
        recovery_admissions: std::sync::Mutex::new(VecDeque::new()),
        config: Arc::new(config),
        providers: Arc::new(providers),
        tools: std::sync::RwLock::new(tools),
        mcp: pacode_mcp::McpPool::new(Default::default(), None, None),
        plugins: Arc::new(pacode_plugin::PluginHost::new()),
        app_version: "test".into(),
        skills: Arc::new(pacode_skills::SkillRegistry::default()),
        scheduler: crate::schedule::Scheduler::new(),
        model_flags: std::sync::RwLock::new(HashMap::new()),
        global_events: crate::transcript::EventSink::new(),
        idle_watch: crate::core::IdleWatch::default(),
    });
    (temp, session, agent, session_id)
}

async fn barrier_wait(barrier: &Barrier) {
    tokio::time::timeout(Duration::from_secs(3), barrier.wait())
        .await
        .expect("barrier timed out");
}

fn user_texts(request: &CompletionRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .map(|message| message.text())
        .collect()
}

fn durable_user_texts(request: &CompletionRequest) -> Vec<String> {
    user_texts(request)
        .into_iter()
        .filter(|text| matches!(text.as_str(), "A" | "B" | "C"))
        .collect()
}

async fn wait_for_turn_start(
    events: &mut tokio::sync::broadcast::Receiver<(u64, Event)>,
    agent: &AgentId,
) -> TurnId {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match events.recv().await {
                Ok((_, Event::TurnStarted { agent: id, turn })) if id == *agent => return turn,
                Ok(_) => {}
                Err(error) => panic!("event stream closed: {error}"),
            }
        }
    })
    .await
    .unwrap()
}

async fn wait_for_turn_end(
    events: &mut tokio::sync::broadcast::Receiver<(u64, Event)>,
    agent: &AgentId,
    turn: &TurnId,
) -> TurnStop {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match events.recv().await {
                Ok((
                    _,
                    Event::TurnEnded {
                        agent: id,
                        turn: id_turn,
                        stop,
                        ..
                    },
                )) if id == *agent && id_turn == *turn => {
                    return stop;
                }
                Ok(_) => {}
                Err(error) => panic!("event stream closed: {error}"),
            }
        }
    })
    .await
    .unwrap()
}

#[test]
fn compaction_uses_latest_input_instead_of_turn_sum() {
    assert_eq!(compaction_input_tokens(Some(40), 900), 40);
    assert_eq!(compaction_input_tokens(None, 900), 900);
}

/// The store keeps the first terminal write for an admission, so a cancel that
/// landed while the turn was finishing must not be recorded as `completed`.
#[tokio::test(flavor = "multi_thread")]
async fn fenced_admission_finalizes_as_interrupted_even_when_the_turn_completed() {
    let provider = BlockingProvider::new(false);
    let (_temp, _session, agent, _session_id) = test_session(provider).await;
    let admission = "fenced-completion";
    let turn = TurnId::new("trn_fenced-completion");

    assert!(agent.begin_admission_turn(admission.into(), turn.clone()));
    agent.mark_admission_progressed(admission, &turn);
    agent.cancel_admission(admission);
    let finish = agent
        .finish_admission_turn(&turn, &TurnStop::Completed)
        .expect("a fenced admission still finishes through the supervisor");
    assert!(finish.terminal);
    assert!(agent.admission_is_cancelled(admission));

    assert_eq!(
        persisted_terminal_stop(true, &TurnStop::Completed),
        "interrupted"
    );
    assert_eq!(
        persisted_terminal_stop(
            true,
            &TurnStop::Failed {
                message: "boom".into()
            }
        ),
        "interrupted"
    );
    assert_eq!(
        persisted_terminal_stop(false, &TurnStop::Completed),
        "completed"
    );
    assert_eq!(
        persisted_terminal_stop(
            false,
            &TurnStop::Failed {
                message: "boom".into()
            }
        ),
        "failed"
    );
}

/// A claim that failed must leave the admission replayable in the order the
/// store asked for, and must leave work behind that wakes an idle agent —
/// otherwise the message sits unanswered until the next resume.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_claim_parks_the_admission_in_order_and_wakes_the_agent() {
    let provider = BlockingProvider::new(false);
    let (_temp, session, agent, _session_id) = test_session(provider).await;
    let main = AgentId::main();
    session.queue_recovery_admissions(&main, ["earlier".to_string(), "later".to_string()]);

    park_admission_for_recovery(&session, &agent, agent.turn_generation(), "failed-claim");

    assert_eq!(
        session.take_recovery_admissions(&main),
        vec![
            "failed-claim".to_string(),
            "earlier".to_string(),
            "later".to_string()
        ]
    );
    assert!(agent.injections.has_user_admission());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_agent_is_not_woken_by_a_failed_claim() {
    let provider = BlockingProvider::new(false);
    let (_temp, session, agent, _session_id) = test_session(provider).await;
    agent.request_stop();

    park_admission_for_recovery(&session, &agent, agent.turn_generation(), "stopped-claim");

    assert!(agent.injections.is_empty());
    assert!(
        session
            .take_recovery_admissions(&AgentId::main())
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn queued_admissions_get_distinct_fifo_boundaries() {
    let provider = BlockingProvider::new(true);
    let (_temp, session, _agent, _session_id) = test_session(provider.clone()).await;
    let mut events = session.events.subscribe();
    session
        .submit_user_message_with_images_idempotent("first".into(), Vec::new(), "first".into(), 1)
        .await
        .unwrap();
    barrier_wait(&provider.entered).await;
    session
        .submit_user_message_with_images_idempotent("second".into(), Vec::new(), "second".into(), 2)
        .await
        .unwrap();
    barrier_wait(&provider.release).await;

    let first_turn = TurnId::new("trn_first");
    let second_turn = TurnId::new("trn_second");
    assert_eq!(
        wait_for_turn_end(&mut events, &AgentId::main(), &second_turn).await,
        TurnStop::Completed
    );

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(user_texts(&requests[0]).contains(&"first".to_owned()));
    assert!(!user_texts(&requests[0]).contains(&"second".to_owned()));
    assert!(user_texts(&requests[1]).contains(&"second".to_owned()));
    assert_eq!(
        session
            .store
            .get_message_admission(&session.id, "first")
            .await
            .unwrap()
            .unwrap()
            .turn_id,
        Some(first_turn)
    );
    assert_eq!(
        session
            .store
            .get_message_admission(&session.id, "second")
            .await
            .unwrap()
            .unwrap()
            .turn_id,
        Some(second_turn)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_keeps_one_paired_admission_boundary() {
    let provider = BlockingProvider::new(true);
    let (_temp, session, _agent, _session_id) = test_session(provider.clone()).await;
    let mut events = session.events.subscribe();
    session
        .submit_user_message_with_images_idempotent("cancel".into(), Vec::new(), "cancel".into(), 1)
        .await
        .unwrap();
    barrier_wait(&provider.entered).await;
    session.interrupt();
    barrier_wait(&provider.release).await;

    let turn = TurnId::new("trn_cancel");
    assert_eq!(
        wait_for_turn_end(&mut events, &AgentId::main(), &turn).await,
        TurnStop::Interrupted
    );
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(
        session
            .store
            .get_message_admission(&session.id, "cancel")
            .await
            .unwrap()
            .unwrap()
            .state,
        pacode_store::MessageAdmissionState::Terminal
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_racing_recovery_is_terminalized_before_lock_release() {
    let _serial = LIFECYCLE_GATE_TEST_LOCK.lock().await;
    let provider = BlockingProvider::new(true);
    let (_temp, session, agent, _session_id) = test_session(provider.clone()).await;
    session
        .store
        .append_user_message_with_turn(
            &session.id,
            &AgentId::main(),
            0,
            "stop-recovery",
            20,
            Some("trn_stop-recovery"),
            &Message::user("stop recovery"),
        )
        .await
        .unwrap();
    session.queue_recovery_admissions(&AgentId::main(), ["stop-recovery".to_owned()]);
    agent
        .injections
        .push(Injection::SystemNotice("start".into()));
    let mut events = session.events.subscribe();
    request_turn(session.clone(), agent.clone());
    barrier_wait(&provider.entered).await;
    let turn = wait_for_turn_start(&mut events, &AgentId::main()).await;
    let gate = install_lifecycle_gate(&session, agent.as_ref());
    barrier_wait(&provider.release).await;
    timeout(Duration::from_secs(3), gate.reached.acquire())
        .await
        .unwrap()
        .expect("lifecycle gate semaphore closed")
        .forget();

    assert!(agent.is_running());
    agent.request_stop();
    gate.release.add_permits(1);
    assert_eq!(
        wait_for_turn_end(&mut events, &AgentId::main(), &turn).await,
        TurnStop::Completed
    );
    assert!(!agent.is_running());
    assert!(!agent.injections.has_user_admission());
    assert_eq!(
        session
            .store
            .get_message_admission(&session.id, "stop-recovery")
            .await
            .unwrap()
            .unwrap()
            .state,
        pacode_store::MessageAdmissionState::Terminal
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_admissions_preserve_fifo_before_live_admissions() {
    let provider = BlockingProvider::new(true);
    let (_temp, session, agent, _session_id) = test_session(provider.clone()).await;
    for (seq, admission, text) in [(0, "recovery-a", "A"), (1, "recovery-b", "B")] {
        let turn_id = format!("trn_{admission}");
        session
            .store
            .append_user_message_with_turn(
                &session.id,
                &AgentId::main(),
                seq,
                admission,
                seq + 10,
                Some(turn_id.as_str()),
                &Message::user(text),
            )
            .await
            .unwrap();
    }
    session.queue_recovery_admissions(
        &AgentId::main(),
        ["recovery-a".to_owned(), "recovery-b".to_owned()],
    );
    agent
        .injections
        .push(Injection::SystemNotice("start".into()));
    let mut events = session.events.subscribe();
    request_turn(session.clone(), agent.clone());
    barrier_wait(&provider.entered).await;
    session
        .submit_user_message_with_images_idempotent("C".into(), Vec::new(), "live-c".into(), 12)
        .await
        .unwrap();
    barrier_wait(&provider.release).await;

    wait_for_turn_end(&mut events, &AgentId::main(), &TurnId::new("trn_live-c")).await;
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(durable_user_texts(&requests[1]), vec!["A".to_owned()]);
    assert_eq!(
        durable_user_texts(&requests[2]),
        vec!["A".to_owned(), "B".to_owned()]
    );
    assert_eq!(
        durable_user_texts(&requests[3]),
        vec!["A".to_owned(), "B".to_owned(), "C".to_owned()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn store_failure_does_not_hot_loop_failed_injection() {
    let provider = BlockingProvider::new(false);
    let (_temp, session, agent, _session_id) = test_session(provider.clone()).await;
    agent
        .injections
        .push(Injection::SystemNotice("blocked".into()));
    let write_guard = agent.message_write_lock.clone().lock_owned().await;
    let mut events = session.events.subscribe();
    session
        .submit_user_message_with_images_idempotent(
            "store failure".into(),
            Vec::new(),
            "store-failure".into(),
            1,
        )
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let admission = session
                .store
                .get_message_admission(&session.id, "store-failure")
                .await
                .unwrap()
                .unwrap();
            if admission.state == pacode_store::MessageAdmissionState::Running {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    session.store.set_fault_injection(true);
    drop(write_guard);
    assert!(matches!(
        wait_for_turn_end(
            &mut events,
            &AgentId::main(),
            &TurnId::new("trn_store-failure")
        )
        .await,
        TurnStop::Failed { .. }
    ));
    session.store.set_fault_injection(false);
    assert!(provider.requests().is_empty());
    assert!(!agent.injections.has_user_admission());
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_finalization_precedes_lock_release() {
    let _serial = LIFECYCLE_GATE_TEST_LOCK.lock().await;
    let provider = BlockingProvider::new(true);
    let (_temp, session, main, _session_id) = test_session(provider.clone()).await;
    main.request_stop();
    let sub_id = AgentId::new("agt-finalization");
    let now = pacode_types::now_ms();
    let mut history = History::default();
    history.push(Message::user("work"));
    let sub = Arc::new(Agent::new(
        sub_id.clone(),
        AgentInfo {
            id: sub_id.clone(),
            name: "finalization".into(),
            kind: AgentKind::Sub,
            status: AgentStatus::Idle,
            activity: None,
            started_at_ms: now,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model: ModelRoute::new("barrier", "mock-model"),
            effort: pacode_types::Effort::Medium,
            parent: Some(AgentId::main()),
            summary: None,
            error: None,
        },
        history,
        ToolRegistry::new(),
        200,
        Some("work".into()),
    ));
    session
        .agents
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(sub_id.clone(), sub.clone());
    let mut events = session.events.subscribe();
    start_turn(session.clone(), sub.clone());
    barrier_wait(&provider.entered).await;
    let turn = wait_for_turn_start(&mut events, &sub_id).await;
    let gate = install_lifecycle_gate(&session, sub.as_ref());
    barrier_wait(&provider.release).await;
    timeout(Duration::from_secs(3), gate.reached.acquire())
        .await
        .unwrap()
        .expect("lifecycle gate semaphore closed")
        .forget();

    assert_eq!(sub.status(), AgentStatus::Finished);
    assert!(sub.is_running());
    assert!(sub.history.lock().unwrap().messages.is_empty());

    gate.release.add_permits(1);
    wait_for_turn_end(&mut events, &sub_id, &turn).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deferred_admission_is_hidden_then_restored() {
    let provider = BlockingProvider::new(false);
    let (_temp, session, agent, _session_id) = test_session(provider).await;
    let first = agent.reserve_user_message_with_admission(Message::user("first"), "first".into());
    session
        .store
        .append_user_message_with_turn(
            &session.id,
            &AgentId::main(),
            first.seq(),
            "first",
            1,
            Some("trn_first"),
            first.message(),
        )
        .await
        .unwrap();
    first.persist();
    let second =
        agent.reserve_user_message_with_admission(Message::user("second"), "second".into());
    session
        .store
        .append_user_message_with_turn(
            &session.id,
            &AgentId::main(),
            second.seq(),
            "second",
            2,
            Some("trn_second"),
            second.message(),
        )
        .await
        .unwrap();
    second.persist();
    agent.commit_pending_user_messages();
    hide_other_admission_messages(&session, agent.as_ref(), Some("first"))
        .await
        .unwrap();
    let visible = agent
        .history
        .lock()
        .unwrap()
        .messages
        .iter()
        .map(|message| message.text())
        .collect::<Vec<_>>();
    assert_eq!(visible, vec!["first".to_owned()]);

    let admission = session
        .store
        .get_message_admission(&session.id, "second")
        .await
        .unwrap()
        .unwrap();
    restore_admission_message(&session, agent.as_ref(), &admission)
        .await
        .unwrap();
    let restored = agent
        .history
        .lock()
        .unwrap()
        .messages
        .iter()
        .map(|message| message.text())
        .collect::<Vec<_>>();
    assert_eq!(restored, vec!["first".to_owned(), "second".to_owned()]);
}

/// An injection that lands in the window between a turn deciding it has
/// nothing to redrive and that turn releasing its lock must still be delivered.
///
/// The window is real: `should_redrive` is computed before the end-of-turn
/// store write, and the lock is only released after it. An injector that
/// arrives in between sees the agent as running — which is exactly what
/// `is_running` reports, because the turn still holds the lock — and skips the
/// wake. If the finished turn then trusts its own earlier `should_redrive`,
/// the injection is stranded: the queue is non-empty, the agent is idle, and
/// nothing will start a turn until some unrelated event happens to.
///
/// At 128 subagents this is the difference between a slow session and a stuck
/// one, so the fix belongs in the turn, not in the injector.
#[tokio::test(flavor = "multi_thread")]
async fn an_injection_landing_in_the_teardown_window_is_not_stranded() {
    let _serial = LIFECYCLE_GATE_TEST_LOCK.lock().await;
    let provider = BlockingProvider::new(true);
    let (_temp, session, agent, _session_id) = test_session(provider.clone()).await;
    let mut events = session.events.subscribe();

    start_turn(session.clone(), agent.clone());
    barrier_wait(&provider.entered).await;
    let turn = wait_for_turn_start(&mut events, &AgentId::main()).await;
    // Park the turn inside its own teardown, holding the lock: this is the
    // window, made deterministic.
    let gate = install_lifecycle_gate(&session, agent.as_ref());
    barrier_wait(&provider.release).await;
    timeout(Duration::from_secs(3), gate.reached.acquire())
        .await
        .unwrap()
        .expect("lifecycle gate semaphore closed")
        .forget();

    // The turn is now deciding, lock still held, `should_redrive` already false.
    session.inject_and_wake(
        &agent,
        Injection::SystemNotice("landed during teardown".into()),
    );

    gate.release.add_permits(1);
    wait_for_turn_end(&mut events, &AgentId::main(), &turn).await;

    // The stranded case is an idle agent holding an undelivered injection.
    // The delivered case is a second turn that starts and drains it.
    let mut delivered = None;
    for _ in 0..2 {
        match tokio::time::timeout(Duration::from_millis(400), events.recv()).await {
            Ok(Ok((
                _,
                Event::TurnStarted {
                    agent: id,
                    turn: next,
                },
            ))) if id == AgentId::main() => {
                delivered = Some(next);
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let Some(next) = delivered else {
        panic!(
            "injection was stranded: the agent is idle holding an undelivered \
             injection. A turn must re-check its queue after releasing the lock"
        );
    };
    // Delivery, not merely a second turn: the notice has to reach the model.
    // The provider only parks its first call, so this turn runs straight
    // through and its request is the record of what the agent was told.
    wait_for_turn_end(&mut events, &AgentId::main(), &next).await;
    assert!(
        provider.requests().iter().any(|request| request
            .messages
            .iter()
            .any(|message| message.text().contains("landed during teardown"))),
        "the second turn started but the stranded injection was not delivered to it"
    );
}

#[test]
fn terminal_stop_mapping_is_exhaustive() {
    assert_eq!(terminal_stop(&TurnStop::Completed), "completed");
    assert_eq!(terminal_stop(&TurnStop::Interrupted), "interrupted");
    assert_eq!(
        terminal_stop(&TurnStop::Failed {
            message: "failed".into()
        }),
        "failed"
    );
}
