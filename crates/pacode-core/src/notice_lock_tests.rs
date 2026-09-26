//! Tests for the notice lock: the hold/release policy, its caps, the pacing at
//! the drain sites, and the wake that unlocking owes the agents holding.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pacode_provider::{CompletionRequest, EventStream, Provider, ProviderError};
use pacode_tools::ToolRegistry;
use pacode_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, Config, Event, ModelInfo, ModelRoute, Role,
    SessionId, SessionMeta, StopReason, StreamEvent, TaskId, TaskInfo, TaskStatus, Usage,
};

use super::*;
use crate::agent::{Agent, History};
use crate::inject::Injection;
use crate::session::Session;

static SESSION_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn at(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

fn agent_info(id: &str) -> AgentInfo {
    AgentInfo {
        id: AgentId::new(id),
        name: id.to_string(),
        kind: AgentKind::Sub,
        status: AgentStatus::Idle,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: pacode_types::Effort::Low,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    }
}

fn task_info(id: &str) -> TaskInfo {
    TaskInfo {
        id: TaskId::new(id),
        session: SessionId::new("sst_test"),
        owner: AgentId::main(),
        label: format!("task {id}"),
        command: "true".into(),
        cwd: PathBuf::from("/tmp"),
        status: TaskStatus::Completed,
        backgrounded: true,
        exit_code: Some(0),
        started_at_ms: 0,
        ended_at_ms: Some(1),
        progress: None,
        warnings: 0,
        errors: 0,
        output_path: PathBuf::from("/tmp/out"),
        output_bytes: 0,
        acked: false,
    }
}

fn agent_finished(agent: &str, answer: &str) -> Injection {
    Injection::AgentFinished {
        agent: agent_info(agent),
        answer: answer.to_string(),
    }
}

fn subagent_report(agent: &str, text: &str) -> Injection {
    Injection::AgentStatus {
        agent: agent_info(agent),
        text: text.to_string(),
    }
}

fn task_finished(task: &str, tail: &str) -> Injection {
    Injection::TaskFinished {
        task: task_info(task),
        tail: tail.to_string(),
    }
}

fn task_stalled(task: &str, tail: &str) -> Injection {
    Injection::TaskStalled {
        task: task_info(task),
        tail: tail.to_string(),
    }
}

fn agent() -> Agent {
    Agent::new(
        AgentId::main(),
        {
            let mut info = agent_info("main");
            info.kind = AgentKind::Main;
            info.parent = None;
            info
        },
        History::default(),
        ToolRegistry::new(),
        200,
        None,
    )
}

/// The injections a `take_due` released, in delivery order.
fn texts(taken: &Taken) -> Vec<String> {
    taken
        .items
        .iter()
        .map(|item| match &item.injection {
            Injection::AgentFinished { answer, .. } => answer.clone(),
            Injection::AgentStatus { text, .. } => text.clone(),
            Injection::TaskFinished { tail, .. } | Injection::TaskStalled { tail, .. } => {
                tail.clone()
            }
            Injection::SystemNotice(text) => text.clone(),
            Injection::UserSteer { admission } => {
                format!("steer {}", admission.as_deref().unwrap_or("none"))
            }
            Injection::StatusRequest => "status".to_string(),
        })
        .collect()
}

#[test]
fn a_holdable_injection_while_locked_is_parked_and_never_queued() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);

    assert!(agent_finished("a1", "done").is_holdable());
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&agent_finished("a1", "done"))
    );
    assert!(agent.notice_lock.hold_if_locked(&task_finished("t1", "ok")));
    assert_eq!(agent.notice_lock.held(), 2);
    assert!(
        agent.injections.is_empty(),
        "a parked notice reached the queue"
    );
    // A release attempt while the switch is on gets nothing, however long it
    // waits, and arms no deadline to wait for.
    assert!(agent.take_due(at(now, 10_000)).is_none());
    assert!(!agent.notice_lock.wants_work());
    assert_eq!(agent.next_release_in(at(now, 10_000)), None);
}

#[test]
fn a_user_steer_and_a_status_request_always_reach_the_live_queue() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);

    let steer = Injection::UserSteer {
        admission: Some("msg_1".into()),
    };
    assert!(!steer.is_holdable());
    assert!(!agent.notice_lock.hold_if_locked(&steer));
    assert!(!Injection::StatusRequest.is_holdable());
    assert!(!agent.notice_lock.hold_if_locked(&Injection::StatusRequest));
    assert_eq!(agent.notice_lock.held(), 0);
    assert_eq!(agent.notice_lock.dropped(), 0);

    // And they are not stranded behind the switch: a locked agent still
    // delivers live traffic, or the user waits on a turn that never starts.
    agent.injections.push(steer);
    agent.injections.push(Injection::StatusRequest);
    let taken = agent
        .take_due(at(now, 10_000))
        .expect("live traffic is due");
    assert_eq!(texts(&taken), vec!["steer msg_1", "status"]);
    assert!(
        taken
            .items
            .iter()
            .all(|item| item.origin == DueOrigin::Queue)
    );
}

#[test]
fn take_due_releases_one_parked_item_per_interval_and_then_live_items_unpaced() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    for i in 0..2 {
        assert!(
            agent
                .notice_lock
                .hold_if_locked(&subagent_report("a1", &format!("held {i}")))
        );
    }
    agent.notice_lock.set_locked(false, now);

    // First release is immediate — a single held notice must not pay a delay.
    let first = agent.take_due(now).expect("first release is immediate");
    assert_eq!(texts(&first), vec!["held 0"]);
    assert_eq!(first.items[0].origin, DueOrigin::Held);
    assert!(agent.notice_lock.wants_work());
    for ms in [1, 100, DRAIN_INTERVAL_MS - 1] {
        assert!(
            agent.take_due(at(now, ms)).is_none(),
            "released early at {ms} ms"
        );
    }
    assert_eq!(agent.notice_lock.held(), 1);

    // Live traffic queues behind the paced one and comes out with it, unpaced.
    agent.injections.push(agent_finished("a2", "live"));
    let second = agent
        .take_due(at(now, DRAIN_INTERVAL_MS))
        .expect("second release");
    assert_eq!(texts(&second), vec!["held 1", "live"]);
    assert_eq!(second.items[0].origin, DueOrigin::Held);
    assert_eq!(second.items[1].origin, DueOrigin::Queue);

    // Nothing left: no deadline, no work, and no release ever again.
    assert!(!agent.notice_lock.wants_work());
    assert_eq!(agent.next_release_in(at(now, DRAIN_INTERVAL_MS * 3)), None);
    assert!(agent.take_due(at(now, DRAIN_INTERVAL_MS * 3)).is_none());
}

#[test]
fn restore_due_is_a_complete_inverse_of_take_due() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    for i in 0..3 {
        assert!(
            agent
                .notice_lock
                .hold_if_locked(&subagent_report("a1", &format!("held {i}")))
        );
    }
    agent.injections.push(agent_finished("a2", "live"));
    agent.notice_lock.set_locked(false, now);

    let deadline_before = agent.next_release_in(now);
    let taken = agent.take_due(now).expect("first release is immediate");
    assert_eq!(texts(&taken), vec!["held 0", "live"]);
    // The take spent the release: the next item is 250 ms out.
    assert_eq!(
        agent.next_release_in(at(now, 1)),
        Some(Duration::from_millis(DRAIN_INTERVAL_MS - 1))
    );

    agent.restore_due(taken);

    assert_eq!(
        agent.notice_lock.held(),
        3,
        "a restored item left the backlog"
    );
    assert_eq!(agent.next_release_in(now), deadline_before);
    // The §4 failure: the deadline stayed advanced, so the release that was
    // handed back could never come out again and the backlog starved.
    let again = agent
        .take_due(now)
        .expect("the restored release is due again");
    assert_eq!(texts(&again), vec!["held 0", "live"]);
    agent.restore_due(again);
    assert_eq!(agent.notice_lock.held(), 3);

    // The order across a restore is the order the agent had: the backlog first,
    // and the live item behind the release it went out with.
    let mut drained = Vec::new();
    let mut clock = now;
    while let Some(taken) = agent.take_due(clock) {
        drained.extend(texts(&taken));
        clock += Duration::from_millis(DRAIN_INTERVAL_MS);
    }
    assert_eq!(
        drained,
        vec!["held 0", "live", "held 1", "held 2"],
        "a restore reordered the backlog"
    );
}

#[test]
fn a_lock_with_nothing_parked_arms_no_timer() {
    let lock = NoticeLock::new();
    let now = Instant::now();
    lock.set_locked(true, now);
    assert!(!lock.wants_work());
    assert_eq!(lock.next_release_in(now), None);
    lock.set_locked(false, now);
    assert!(!lock.wants_work());
    assert_eq!(lock.next_release_in(at(now, 10_000)), None);
    assert!(lock.badge().is_none());
}

#[test]
fn relocking_pauses_the_release_and_resuming_keeps_the_wait_left() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&subagent_report("a1", "one"))
    );
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&subagent_report("a2", "two"))
    );
    agent.notice_lock.set_locked(false, now);
    assert!(agent.take_due(now).is_some());

    // Re-locked 100 ms in: the remaining 150 ms of the wait is not forgiven.
    agent.notice_lock.set_locked(true, at(now, 100));
    assert!(agent.take_due(at(now, 10_000)).is_none());
    agent.notice_lock.set_locked(false, at(now, 200));
    assert!(agent.take_due(at(now, 200)).is_none());
    assert!(agent.take_due(at(now, DRAIN_INTERVAL_MS - 1)).is_none());
    assert!(agent.take_due(at(now, DRAIN_INTERVAL_MS)).is_some());
}

#[test]
fn a_stall_and_a_finish_of_the_same_task_never_coalesce() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    // Same task, so the same source and the same per-source cap — and the
    // coalescing subject is the payload, which differs.
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&task_stalled("t1", "no output for 5m"))
    );
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&task_finished("t1", "exited 0"))
    );
    assert_eq!(agent.notice_lock.held(), 2);

    agent.notice_lock.set_locked(false, now);
    let mut clock = now;
    let mut drained = Vec::new();
    while let Some(taken) = agent.take_due(clock) {
        drained.extend(texts(&taken));
        clock += Duration::from_millis(DRAIN_INTERVAL_MS);
    }
    assert_eq!(drained, vec!["no output for 5m", "exited 0"]);
}

#[test]
fn system_notices_are_capped_by_the_shared_system_bucket() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    for i in 0..=PER_SOURCE_CAP {
        assert!(
            agent
                .notice_lock
                .hold_if_locked(&Injection::SystemNotice(format!("n{i}")))
        );
    }
    // "No source" is a source: a chatty monitor stream cannot grow the backlog
    // past the per-source cap just by having no id to count against.
    assert_eq!(agent.notice_lock.held(), PER_SOURCE_CAP);
    assert_eq!(agent.notice_lock.dropped(), 1);

    agent.notice_lock.set_locked(false, now);
    let mut clock = now;
    let mut drained = Vec::new();
    while let Some(taken) = agent.take_due(clock) {
        drained.extend(texts(&taken));
        clock += Duration::from_millis(DRAIN_INTERVAL_MS);
    }
    assert_eq!(drained[0], "n1", "the oldest system notice was kept");
    assert_eq!(drained.len(), PER_SOURCE_CAP);
}

#[test]
fn the_per_source_cap_evicts_that_sources_oldest_and_spares_the_others() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    for i in 0..=PER_SOURCE_CAP {
        assert!(
            agent
                .notice_lock
                .hold_if_locked(&subagent_report("chatty", &format!("say {i}")))
        );
    }
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&subagent_report("quiet", "only one"))
    );
    assert_eq!(agent.notice_lock.held(), PER_SOURCE_CAP + 1);
    assert_eq!(agent.notice_lock.dropped(), 1);

    agent.notice_lock.set_locked(false, now);
    let mut clock = now;
    let mut drained = Vec::new();
    while let Some(taken) = agent.take_due(clock) {
        drained.extend(texts(&taken));
        clock += Duration::from_millis(DRAIN_INTERVAL_MS);
    }
    assert_eq!(drained[0], "say 1", "the chatty source kept its newest");
    assert!(
        drained.contains(&"only one".to_string()),
        "the quiet source starved"
    );
}

#[test]
fn the_overall_cap_drops_the_arrival_and_counts_it() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    for i in 0..QUEUE_CAP {
        assert!(
            agent
                .notice_lock
                .hold_if_locked(&subagent_report(&format!("a{i}"), "same answer"))
        );
    }
    assert_eq!(agent.notice_lock.held(), QUEUE_CAP);
    assert_eq!(agent.notice_lock.dropped(), 0);
    // Identical payload, different agent: the id is what the orchestrator needs,
    // so these never coalesce — and this one is over the cap.
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&subagent_report("overflow", "same answer"))
    );
    assert_eq!(agent.notice_lock.held(), QUEUE_CAP);
    assert_eq!(agent.notice_lock.dropped(), 1);
}

#[test]
fn identical_injections_coalesce_into_one_delivery_carrying_their_count() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    for _ in 0..11 {
        assert!(
            agent
                .notice_lock
                .hold_if_locked(&subagent_report("a1", "all done"))
        );
    }
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&subagent_report("a1", "something else"))
    );
    assert_eq!(
        agent.notice_lock.held(),
        2,
        "identical arrivals took a slot each"
    );

    agent.notice_lock.set_locked(false, now);
    let taken = agent.take_due(now).expect("first release");
    assert_eq!(texts(&taken), vec!["[x11] all done"]);
    let taken = agent
        .take_due(at(now, DRAIN_INTERVAL_MS))
        .expect("second release");
    assert_eq!(texts(&taken), vec!["something else"]);
}

#[test]
fn a_session_switch_drops_the_backlog_instead_of_delivering_it_elsewhere() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&subagent_report("a1", "one"))
    );
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&subagent_report("a2", "two"))
    );

    agent.notice_lock.reset();
    assert!(!agent.notice_lock.is_locked());
    assert_eq!(agent.notice_lock.held(), 0);
    assert_eq!(agent.notice_lock.dropped(), 2, "a lossy drop is not hidden");
    assert!(agent.take_due(at(now, 10_000)).is_none());
}

#[test]
fn the_badge_says_what_is_held_and_whether_injections_were_lost() {
    let agent = agent();
    let now = Instant::now();
    agent.notice_lock.set_locked(true, now);
    assert_eq!(
        agent.notice_lock.badge(),
        Some(LockBadge::Holding {
            held: 0,
            dropped: 0
        })
    );
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&subagent_report("a1", "one"))
    );
    assert!(
        agent
            .notice_lock
            .hold_if_locked(&subagent_report("a2", "two"))
    );
    assert_eq!(
        agent.notice_lock.stats_line(),
        "notice lock: on, holding 2 notices"
    );
    assert_eq!(
        LockBadge::Holding {
            held: 2,
            dropped: 0
        }
        .label(),
        "LOCK 2"
    );

    agent.notice_lock.set_locked(false, now);
    assert_eq!(
        agent.notice_lock.badge(),
        Some(LockBadge::Draining { left: 2 })
    );
    assert_eq!(
        agent.notice_lock.stats_line(),
        "notice lock: off, draining 2 notices"
    );

    agent.notice_lock.discard_held();
    assert!(agent.notice_lock.badge().is_none());
    assert_eq!(agent.notice_lock.stats_line(), "notice lock: off");
}

#[test]
fn toggling_records_when_the_lock_was_set_and_twice_is_a_no_op() {
    let lock = NoticeLock::new();
    let now = Instant::now();
    assert!(lock.set_locked(true, now));
    assert!(lock.is_locked());
    assert_eq!(lock.locked_since(), Some(now));
    let later = at(now, 5_000);
    assert!(!lock.toggle(later));
    assert_eq!(lock.locked_since(), None);
    // A second switch-on must not move the timestamp.
    assert!(lock.toggle(later));
    assert!(!lock.set_locked(true, at(later, 1_000)));
    assert_eq!(lock.locked_since(), Some(later));
}

// --- the daemon side: the wake a toggle owes the agents holding -------------

struct CountingProvider {
    calls: AtomicUsize,
    requests: std::sync::Mutex<Vec<CompletionRequest>>,
}

#[async_trait]
impl Provider for CountingProvider {
    fn id(&self) -> &str {
        "counting"
    }

    async fn complete(&self, request: CompletionRequest) -> Result<EventStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request);
        let events = vec![
            Ok(StreamEvent::TextDelta {
                text: "ok".to_string(),
            }),
            Ok(StreamEvent::Usage(Usage::default())),
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
            route: ModelRoute::new("counting", "mock-model"),
            display_name: "Counting".into(),
            context_window: Some(128_000),
            supports_reasoning: false,
            supports_vision: false,
            supported_efforts: None,
            pricing: None,
        }
    }
}

async fn test_session() -> (
    tempfile::TempDir,
    Arc<Session>,
    Arc<Agent>,
    Arc<CountingProvider>,
) {
    let temp = tempfile::tempdir().unwrap();
    let sequence = SESSION_SEQUENCE.fetch_add(1, Ordering::SeqCst);
    let session_id = SessionId::new(format!("notice-lock-tests-{sequence}"));
    let agent_id = AgentId::main();
    let now = pacode_types::now_ms();
    let route = ModelRoute::new("counting", "mock-model");
    let meta = SessionMeta {
        id: session_id.clone(),
        name: Some("notice lock tests".into()),
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
    let provider = Arc::new(CountingProvider {
        calls: AtomicUsize::new(0),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let mut providers = pacode_provider::ProviderRegistry::empty();
    providers.insert(provider.clone());
    providers.set_default_route(Some(ModelRoute::new("counting", "mock-model")));
    let mut config = Config::default();
    config.provider.default = Some("counting/mock-model".into());
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
        accounting: std::sync::Arc::new(crate::analyze::SessionAccounting::new()),
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
    (temp, session, agent, provider)
}

fn user_texts(provider: &CountingProvider) -> Vec<String> {
    provider
        .requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .flat_map(|request: &CompletionRequest| {
            request
                .messages
                .iter()
                .filter(|message| message.role == Role::User)
                .map(|message| message.text())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parked_push_starts_no_turn_and_unlocking_re_drives_the_agent() {
    let (_temp, session, agent, provider) = test_session().await;
    assert!(session.set_notice_lock(true).await);

    session.inject_and_wake(&agent, agent_finished("a1", "the subagent is done"));
    // A turn woken by a parked notice reads an empty injection set: a whole
    // model round trip spent learning nothing, which is the waste the lock
    // exists to prevent.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "a parked push ran a turn"
    );
    assert_eq!(agent.notice_lock.held(), 1);
    assert!(agent.injections.is_empty());

    // The toggle arrives over the wire with no push behind it, so unlocking is
    // the only thing that can wake the agent holding the notice.
    assert!(session.set_notice_lock(false).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "unlock re-drove nothing"
    );
    let delivered = user_texts(&provider).join("\n");
    assert!(
        delivered.contains("the subagent is done"),
        "got {delivered}"
    );
    assert_eq!(agent.notice_lock.held(), 0);
    // A second toggle with nothing held is a no-op, not a second turn.
    assert!(!session.set_notice_lock(false).await);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_badge_event_fires_on_the_flip_and_on_a_count_that_moves_only() {
    let (_temp, session, agent, _provider) = test_session().await;
    let mut events = session.events.subscribe();

    session.set_notice_lock(true).await;
    assert!(matches!(
        events.try_recv(),
        Ok((
            _,
            Event::NoticeLockChanged {
                locked: true,
                held: 0,
                ..
            }
        ))
    ));

    for i in 0..3 {
        session.inject_and_wake(&agent, subagent_report("a1", &format!("say {i}")));
    }
    // A three-notice backlog is three events, one per count that moved, and the
    // last one reports the count the badge shows.
    let mut last = None;
    for _ in 0..3 {
        match events.try_recv() {
            Ok((_, event @ Event::NoticeLockChanged { .. })) => last = Some(event),
            other => panic!("expected a notice-lock event, got {other:?}"),
        }
    }
    assert!(matches!(
        last,
        Some(Event::NoticeLockChanged { held: 3, .. })
    ));
    assert!(
        events.try_recv().is_err(),
        "a fourth event for three notices"
    );

    // An identical repeat coalesces: the count the badge shows does not move,
    // so nothing is sent.
    session.inject_and_wake(&agent, subagent_report("a1", "say 0"));
    assert!(events.try_recv().is_err());
    assert_eq!(agent.notice_lock.held(), 3);
}
