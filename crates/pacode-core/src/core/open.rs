//! Session opening logic: Attach::New, Attach::Resume, Attach::Latest.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use pacode_types::{Attach, SessionId};

use crate::CoreError;
use crate::core::{Core, SessionLease};
use crate::session::Session;

const ATTACH_LEASE_ATTEMPTS: usize = 3;

impl Core {
    pub async fn open_session_with_id(
        &self,
        attach: Attach,
        session_id: SessionId,
    ) -> Result<SessionId, CoreError> {
        if let Attach::Latest { cwd } = &attach {
            return open_latest_with_id(self, cwd.clone(), session_id).await;
        }
        let lifecycle_lock = self.session_lock(&session_id);
        let _lifecycle_guard = lifecycle_lock.lock().await;
        let idempotent_new = matches!(&attach, Attach::New { .. });
        let opened = open_session_unlocked(self, attach, Some(session_id.clone())).await?;
        if idempotent_new {
            return Ok(self.bind_idempotent_session(&session_id, opened));
        }
        Ok(opened)
    }

    pub async fn open_attached(
        &self,
        attach: Attach,
    ) -> Result<(SessionId, SessionLease), CoreError> {
        let mut attempts_left = ATTACH_LEASE_ATTEMPTS;
        loop {
            let id = open_session(self, attach.clone()).await?;
            attempts_left -= 1;
            match self.attach_lease(&id).await {
                Ok(lease) => return Ok((id, lease)),
                Err(error) if attempts_left == 0 => return Err(error),
                Err(_) => {}
            }
        }
    }

    fn idempotent_session(&self, identity: &SessionId) -> Option<SessionId> {
        self.idempotent_sessions
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(identity)
            .cloned()
    }

    fn bind_idempotent_session(&self, identity: &SessionId, session: SessionId) -> SessionId {
        self.idempotent_sessions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(identity.clone())
            .or_insert(session)
            .clone()
    }
}

async fn open_latest_with_id(
    core: &Core,
    cwd: PathBuf,
    identity: SessionId,
) -> Result<SessionId, CoreError> {
    if let Some(session) = core.idempotent_session(&identity) {
        return open_session(core, Attach::Resume { session }).await;
    }
    if core.session(&identity).is_some() || core.deps.store.get_session(&identity).await?.is_some()
    {
        let session = open_session(
            core,
            Attach::Resume {
                session: identity.clone(),
            },
        )
        .await?;
        return Ok(core.bind_idempotent_session(&identity, session));
    }

    let filter = pacode_store::SessionFilter {
        cwd: Some(cwd.clone()),
        limit: 1,
    };
    let selected = core
        .deps
        .store
        .list_sessions(filter)
        .await?
        .into_iter()
        .next();
    let Some(selected) = selected else {
        let lifecycle_lock = core.session_lock(&identity);
        let _lifecycle_guard = lifecycle_lock.lock().await;
        let session = open_session_unlocked(
            core,
            Attach::New {
                cwd,
                model: None,
                effort: None,
                mode: None,
            },
            Some(identity.clone()),
        )
        .await?;
        return Ok(core.bind_idempotent_session(&identity, session));
    };
    let session = open_session(
        core,
        Attach::Resume {
            session: selected.id,
        },
    )
    .await?;
    Ok(core.bind_idempotent_session(&identity, session))
}

pub(crate) async fn open_session(core: &Core, attach: Attach) -> Result<SessionId, CoreError> {
    // Timed here so the reading covers the whole open, lock included: a resume
    // that waits behind another attach is part of what the user waits for.
    let started = std::time::Instant::now();
    let opened = if let Attach::Resume { session } = &attach {
        let lifecycle_lock = core.session_lock(session);
        let _lifecycle_guard = lifecycle_lock.lock().await;
        open_session_unlocked(core, attach, None).await
    } else {
        open_session_unlocked(core, attach, None).await
    };
    core.record_resume(started.elapsed());
    opened
}

const MAIN_HISTORY_PAGES: u64 = 2;
const SUB_HISTORY_PAGES: u64 = 1;
const RESUME_WINDOW_FLOOR: u64 = 128;
const RESUME_WINDOW_CEILING: u64 = 2048;

fn resume_window(history_page: u32, pages: u64) -> u64 {
    (history_page as u64)
        .saturating_mul(pages)
        .clamp(RESUME_WINDOW_FLOOR, RESUME_WINDOW_CEILING)
}

struct HydratedHistory {
    history: crate::agent::History,
    rows: Vec<(u64, Arc<pacode_types::Message>)>,
}

async fn hydrate_agent_history(
    store: &pacode_store::Store,
    session_id: &SessionId,
    agent_id: &pacode_types::AgentId,
    compaction: Option<(String, u64)>,
    window: u64,
    prompt_fallback: Option<String>,
) -> Result<HydratedHistory, CoreError> {
    let (summary, upto_seq) = match compaction {
        Some((summary, upto_seq)) => (Some(summary), Some(upto_seq)),
        None => (None, None),
    };
    let max_seq = store.max_message_seq(session_id, agent_id).await?;
    let tail_start = max_seq.map(|max_seq| max_seq.saturating_sub(window.saturating_sub(1)));
    let after = match (tail_start, upto_seq) {
        (Some(tail_start), Some(upto_seq)) => Some(tail_start.max(upto_seq)),
        (tail_start, upto_seq) => tail_start.or(upto_seq),
    }
    .filter(|seq| *seq > 0);
    let page = store
        .load_messages_after(session_id, agent_id, after, window as u32)
        .await?;
    let cancelled_seqs = store.cancelled_message_seqs(session_id, agent_id).await?;
    let mut rows: Vec<(u64, Arc<pacode_types::Message>)> = page
        .rows
        .into_iter()
        .filter(|row| !cancelled_seqs.contains(&row.seq))
        .map(|row| (row.seq, Arc::new(row.message)))
        .collect();
    if page.has_more && rows.len() > window as usize {
        rows.drain(..rows.len() - window as usize);
    }
    let mut next_seq = max_seq
        .map(|seq| seq.saturating_add(1))
        .or_else(|| upto_seq.map(|upto_seq| upto_seq.saturating_add(1)))
        .unwrap_or(0);
    if rows.is_empty()
        && let Some(prompt) = prompt_fallback
    {
        // The prompt is a real message of this agent: it takes the seq the next
        // persisted message would, and moves the cursor past it. Parking it on
        // seq 0 instead left the cursor pointing back at it, and the agent's
        // first answer then overwrote the prompt it was answering.
        rows.push((next_seq, Arc::new(pacode_types::Message::user(prompt))));
        next_seq = next_seq.saturating_add(1);
    }
    let estimated_tokens = rows
        .iter()
        .map(|(_, message)| message.estimate_tokens())
        .sum::<u32>()
        .saturating_add(
            summary
                .as_deref()
                .map(pacode_types::estimate_tokens)
                .unwrap_or(0),
        );
    let history = crate::agent::History {
        message_seqs: rows.iter().map(|(seq, _)| *seq).collect(),
        free_seqs: Default::default(),
        messages: rows.iter().map(|(_, message)| message.clone()).collect(),
        next_seq,
        summary,
        summary_upto: upto_seq.unwrap_or(0),
        estimated_tokens,
    };
    Ok(HydratedHistory { history, rows })
}

async fn open_session_unlocked(
    core: &Core,
    attach: Attach,
    preallocated_id: Option<SessionId>,
) -> Result<SessionId, CoreError> {
    match attach {
        Attach::Latest { cwd } => {
            let filter = pacode_store::SessionFilter {
                cwd: Some(cwd.clone()),
                limit: 1,
            };
            let list = core.deps.store.list_sessions(filter).await?;
            if let Some(first) = list.into_iter().next() {
                Box::pin(open_session(core, Attach::Resume { session: first.id })).await
            } else {
                Box::pin(open_session(
                    core,
                    Attach::New {
                        cwd,
                        model: None,
                        effort: None,
                        mode: None,
                    },
                ))
                .await
            }
        }
        Attach::New {
            cwd,
            model,
            effort,
            mode,
        } => {
            if let Some(id) = preallocated_id.as_ref()
                && (core.session(id).is_some() || core.deps.store.get_session(id).await?.is_some())
            {
                return Box::pin(open_session_unlocked(
                    core,
                    Attach::Resume {
                        session: id.clone(),
                    },
                    None,
                ))
                .await;
            }
            let model_route = model
                .or_else(|| core.providers().default_route())
                .or_else(|| core.config().default_route())
                .ok_or(CoreError::NoModel)?;
            let effort = effort.unwrap_or(core.config().provider.effort);
            let mode = mode.unwrap_or(core.config().permissions.default_mode);
            let git_branch = crate::prompt::git_branch(&cwd);
            let id = preallocated_id.unwrap_or_else(SessionId::generate);
            let now = pacode_types::now_ms();

            let meta = pacode_types::SessionMeta {
                id: id.clone(),
                name: None,
                cwd: cwd.clone(),
                git_branch,
                created_at_ms: now,
                updated_at_ms: now,
                model: model_route.clone(),
                effort,
                mode,
                first_prompt: None,
                zdr: false,
            };
            core.deps.store.upsert_session(&meta).await?;

            // Attach must not wait on MCP servers starting: the session opens
            // with schema-cached MCP tools only, and the detached refresh
            // swaps the full set into `session.tools`/agents once it lands.
            let mut session_tools = core.deps.tools.clone();
            for tool in core.get_mcp_tools_cached() {
                session_tools.register(tool);
            }

            let main_info = pacode_types::AgentInfo {
                id: pacode_types::AgentId::main(),
                name: "main".to_string(),
                kind: pacode_types::AgentKind::Main,
                status: pacode_types::AgentStatus::Idle,
                activity: None,
                started_at_ms: now,
                finished_at_ms: None,
                tokens_in: 0,
                tokens_out: 0,
                model: model_route,
                effort,
                parent: None,
                summary: None,
                error: None,
            };
            core.deps.store.upsert_agent(&id, &main_info, None).await?;

            let main_agent = Arc::new(crate::agent::Agent::new(
                pacode_types::AgentId::main(),
                main_info,
                crate::agent::History::default(),
                session_tools.clone(),
                core.deps.config.session.history_page,
                None,
            ));

            let mut agents = BTreeMap::new();
            agents.insert(pacode_types::AgentId::main(), main_agent);

            let session = Arc::new(Session {
                id: id.clone(),
                meta: RwLock::new(meta),
                agents: RwLock::new(agents),
                agent_aliases: std::sync::Mutex::new(HashMap::new()),
                plan: RwLock::new(pacode_types::Plan::default()),
                usage: RwLock::new(pacode_types::UsageTotals::default()),
                permissions: crate::permissions::PermissionState::default(),
                questions: crate::questions::QuestionState::default(),
                events: crate::transcript::EventSink::new(),
                tasks: core.deps.tasks.clone(),
                store: core.deps.store.clone(),
                submit_lock: tokio::sync::Mutex::new(()),
                closed: std::sync::atomic::AtomicBool::new(false),
                recovery_admissions: std::sync::Mutex::new(VecDeque::new()),
                config: core.config(),
                providers: core.providers(),
                tools: RwLock::new(session_tools),
                mcp: core.deps.mcp.clone(),
                plugins: core.deps.plugins.clone(),
                app_version: core.deps.app_version.clone(),
                skills: core.deps.skills.clone(),
                scheduler: crate::schedule::Scheduler::new(),
                model_flags: RwLock::new(pacode_config::load_prefs(&core.deps.paths).model_flags),
                global_events: core.global_events.clone(),
                idle_watch: crate::core::IdleWatch::attach(core),
            });

            start_scheduler(&session).await;
            if let Ok(mut sessions) = core.sessions.write() {
                if let Some(existing) = sessions.get(&id) {
                    return Ok(existing.id.clone());
                }
                sessions.insert(id.clone(), session);
            }
            core.spawn_mcp_tools_refresh();

            Ok(id)
        }
        Attach::Resume {
            session: session_id,
        } => {
            if let Some(existing) = core.session(&session_id) {
                return Ok(existing.id.clone());
            }

            let meta = core
                .deps
                .store
                .get_session(&session_id)
                .await?
                .ok_or_else(|| CoreError::SessionNotFound(session_id.clone()))?;

            let plan = core
                .deps
                .store
                .load_plan(&session_id)
                .await
                .unwrap_or_default();

            let usage = core
                .deps
                .store
                .usage_totals(&session_id)
                .await
                .unwrap_or_default();

            let stored_agents = core.deps.store.list_agents(&session_id).await?;
            let persisted_tasks = core.deps.store.list_tasks(&session_id).await?;
            let mut hydrated_tasks = Vec::with_capacity(persisted_tasks.len());
            let mut stale_tasks = Vec::new();
            for mut task in persisted_tasks {
                if task.status == pacode_types::TaskStatus::Running {
                    task.status = pacode_types::TaskStatus::Killed;
                    task.ended_at_ms.get_or_insert(pacode_types::now_ms());
                    stale_tasks.push(task.clone());
                }
                hydrated_tasks.push(task);
            }
            core.deps.tasks.hydrate(hydrated_tasks);

            // Same cache-only rule as `Attach::New` — resuming must not start
            // MCP servers on the attach path either.
            let mut session_tools = core.deps.tools.clone();
            for tool in core.get_mcp_tools_cached() {
                session_tools.register(tool);
            }

            let main_id = pacode_types::AgentId::main();
            let compaction = core
                .deps
                .store
                .load_compaction(&session_id, &main_id)
                .await?;
            let main_hydrated = hydrate_agent_history(
                &core.deps.store,
                &session_id,
                &main_id,
                compaction,
                resume_window(core.deps.config.session.history_page, MAIN_HISTORY_PAGES),
                None,
            )
            .await?;
            let main_history = main_hydrated.history;

            // The transcript keeps a bounded tail, so only that tail is worth
            // rendering into items: converting the whole history and letting the
            // cap evict most of it was work thrown away on every resume.
            //
            // Item seqs come from the transcript's own counter and the store
            // boundary from the messages' seqs — two separate domains, kept
            // separate here so `GetHistory` can page the store without ever
            // reading an item seq as a message seq.
            let tail_cap = core.deps.config.session.history_page as usize * 2;
            let tail_start = main_hydrated.rows.len().saturating_sub(tail_cap);
            let tail_rows = &main_hydrated.rows[tail_start..];
            let items = crate::transcript::history_to_items(&main_id, tail_rows);
            let mut transcript_state = crate::transcript::TranscriptState::new(tail_cap);
            // Lift the tail above the band reserved for store history, then
            // number from the transcript's own counter.
            transcript_state.reserve_history_band();
            for entry in items {
                let mut item = entry.item;
                item.seq = transcript_state.next_seq();
                transcript_state.upsert(item);
            }
            // The tail's oldest message is where the store takes over; below it
            // there is nothing, so a conversation that fits entirely in the tail
            // has no floor at all.
            let history_floor = tail_rows
                .first()
                .map(|(seq, _)| *seq)
                .filter(|seq| *seq > 0);

            let mut agents = BTreeMap::new();
            let mut main_info = stored_agents
                .iter()
                .find(|a| a.id.is_main())
                .cloned()
                .unwrap_or_else(|| {
                    let now = pacode_types::now_ms();
                    pacode_types::AgentInfo {
                        id: pacode_types::AgentId::main(),
                        name: "main".to_string(),
                        kind: pacode_types::AgentKind::Main,
                        status: pacode_types::AgentStatus::Idle,
                        activity: None,
                        started_at_ms: now,
                        finished_at_ms: None,
                        tokens_in: 0,
                        tokens_out: 0,
                        model: meta.model.clone(),
                        effort: meta.effort,
                        parent: None,
                        summary: None,
                        error: None,
                    }
                });
            // Meta is authoritative for the session route/effort; agent rows
            // written before `set_model` propagated to agents hold a stale
            // snapshot that would otherwise resume the old model.
            main_info.model = meta.model.clone();
            main_info.effort = meta.effort;
            main_info.status = pacode_types::AgentStatus::Idle;
            main_info.activity = None;
            main_info.error = None;
            main_info.finished_at_ms = None;

            let main_agent = Arc::new(crate::agent::Agent::new_with_transcript(
                pacode_types::AgentId::main(),
                main_info,
                main_history,
                session_tools.clone(),
                transcript_state,
                None,
            ));
            main_agent.seed_history_floor(history_floor);
            agents.insert(pacode_types::AgentId::main(), main_agent);

            let mut stale_subagents = Vec::new();
            for mut sub_info in stored_agents {
                if sub_info.id.is_main() {
                    continue;
                }
                if sub_info.status.is_live() {
                    sub_info.status = pacode_types::AgentStatus::Finished;
                    stale_subagents.push(sub_info.clone());
                }
                let sub_prompt = core
                    .deps
                    .store
                    .agent_prompt(&session_id, &sub_info.id)
                    .await?;
                let sub_compaction = core
                    .deps
                    .store
                    .load_compaction(&session_id, &sub_info.id)
                    .await?;
                let sub_hydrated = hydrate_agent_history(
                    &core.deps.store,
                    &session_id,
                    &sub_info.id,
                    sub_compaction,
                    resume_window(core.deps.config.session.history_page, SUB_HISTORY_PAGES),
                    sub_prompt.clone(),
                )
                .await?;
                let sub_agent = Arc::new(crate::agent::Agent::new(
                    sub_info.id.clone(),
                    sub_info.clone(),
                    sub_hydrated.history,
                    session_tools.clone(),
                    core.deps.config.session.history_page,
                    sub_prompt,
                ));
                agents.insert(sub_info.id, sub_agent);
            }

            let session = Arc::new(Session {
                id: session_id.clone(),
                meta: RwLock::new(meta),
                agents: RwLock::new(agents),
                agent_aliases: std::sync::Mutex::new(HashMap::new()),
                plan: RwLock::new(plan),
                usage: RwLock::new(usage),
                permissions: crate::permissions::PermissionState::default(),
                questions: crate::questions::QuestionState::default(),
                events: crate::transcript::EventSink::new(),
                tasks: core.deps.tasks.clone(),
                store: core.deps.store.clone(),
                submit_lock: tokio::sync::Mutex::new(()),
                closed: std::sync::atomic::AtomicBool::new(false),
                recovery_admissions: std::sync::Mutex::new(VecDeque::new()),
                config: core.config(),
                providers: core.providers(),
                tools: RwLock::new(session_tools),
                mcp: core.deps.mcp.clone(),
                plugins: core.deps.plugins.clone(),
                app_version: core.deps.app_version.clone(),
                skills: core.deps.skills.clone(),
                scheduler: crate::schedule::Scheduler::new(),
                model_flags: RwLock::new(pacode_config::load_prefs(&core.deps.paths).model_flags),
                global_events: core.global_events.clone(),
                idle_watch: crate::core::IdleWatch::attach(core),
            });

            start_scheduler(&session).await;
            let (session, inserted) = {
                let mut sessions = core.sessions.write().unwrap_or_else(|p| p.into_inner());
                insert_if_absent(&mut sessions, session_id.clone(), session)
            };
            if !commit_resume_statuses(
                &core.deps.store,
                &session_id,
                inserted,
                stale_tasks,
                stale_subagents,
            )
            .await
            {
                return Ok(session.id.clone());
            }
            if let Err(error) = session.recover_message_admissions().await {
                log::warn!(
                    "failed to recover message admissions for {}: {error}",
                    session.id
                );
            }
            core.spawn_mcp_tools_refresh();

            Ok(session.id.clone())
        }
    }
}

async fn commit_resume_statuses(
    store: &pacode_store::Store,
    session_id: &SessionId,
    inserted: bool,
    tasks: Vec<pacode_types::TaskInfo>,
    subagents: Vec<pacode_types::AgentInfo>,
) -> bool {
    if !inserted {
        return false;
    }
    for task in tasks {
        let _ = store.upsert_task(&task).await;
    }
    for info in subagents {
        let _ = store.upsert_agent(session_id, &info, None).await;
    }
    true
}

fn insert_if_absent<T>(
    sessions: &mut BTreeMap<SessionId, Arc<T>>,
    id: SessionId,
    candidate: Arc<T>,
) -> (Arc<T>, bool) {
    match sessions.entry(id) {
        std::collections::btree_map::Entry::Occupied(entry) => (entry.get().clone(), false),
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(candidate.clone());
            (candidate, true)
        }
    }
}

/// Load the session's persisted cron jobs and start the one task that fires both
/// them and its monitors. A resumed session keeps the jobs it had; their next run
/// is recomputed from now, so a daemon that was down does not replay every missed
/// firing at once.
async fn start_scheduler(session: &Arc<Session>) {
    let now = pacode_types::time::now_ms();
    match session.store.list_cron_jobs(&session.id).await {
        Ok(jobs) => session.scheduler.load_jobs(jobs, now),
        Err(e) => log::warn!("failed to load cron jobs for {}: {e}", session.id),
    }
    session.scheduler.start(Arc::downgrade(session));
}

#[cfg(test)]
mod tests {
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

        assert!(
            commit_resume_statuses(&store, &session_id, true, vec![killed], vec![finished]).await
        );
        assert_eq!(
            store.list_tasks(&session_id).await.unwrap()[0].status,
            TaskStatus::Killed
        );
        assert_eq!(
            store.list_agents(&session_id).await.unwrap()[0].status,
            AgentStatus::Finished
        );
    }
}
