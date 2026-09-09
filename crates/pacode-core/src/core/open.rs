//! Session opening logic: Attach::New, Attach::Resume, Attach::Latest.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use pacode_types::{Attach, SessionId};

use crate::CoreError;
use crate::core::Core;
use crate::session::Session;

pub(crate) async fn open_session(core: &Core, attach: Attach) -> Result<SessionId, CoreError> {
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
            let model_route = model
                .or_else(|| core.providers().default_route().cloned())
                .or_else(|| core.config().default_route())
                .ok_or(CoreError::NoModel)?;
            let effort = effort.unwrap_or(core.config().provider.effort);
            let mode = mode.unwrap_or(core.config().permissions.default_mode);
            let git_branch = crate::prompt::git_branch(&cwd);
            let id = SessionId::generate();
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
            };
            core.deps.store.upsert_session(&meta).await?;

            let mut session_tools = core.deps.tools.clone();
            let mcp_tools = core.get_mcp_tools().await;
            for tool in mcp_tools {
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

            let main_agent = Arc::new(crate::agent::Agent {
                id: pacode_types::AgentId::main(),
                info: RwLock::new(main_info),
                history: std::sync::Mutex::new(crate::agent::History::default()),
                injections: crate::inject::InjectionQueue::default(),
                tools: RwLock::new(session_tools.clone()),
                cancel: std::sync::Mutex::new(None),
                turn_lock: tokio::sync::Mutex::new(()),
                transcript: std::sync::Mutex::new(crate::transcript::TranscriptState::new(
                    core.deps.config.session.history_page as usize * 2,
                )),
                prompt: None,
                turns: std::sync::Mutex::new(0),
            });

            let mut agents = BTreeMap::new();
            agents.insert(pacode_types::AgentId::main(), main_agent);

            let session = Arc::new(Session {
                id: id.clone(),
                meta: RwLock::new(meta),
                agents: RwLock::new(agents),
                plan: RwLock::new(pacode_types::Plan::default()),
                usage: RwLock::new(pacode_types::UsageTotals::default()),
                permissions: crate::permissions::PermissionState::default(),
                events: crate::transcript::EventSink::new(),
                tasks: core.deps.tasks.clone(),
                store: core.deps.store.clone(),
                config: core.config(),
                providers: core.providers(),
                tools: RwLock::new(session_tools),
                mcp: core.deps.mcp.clone(),
                plugins: core.deps.plugins.clone(),
                app_version: core.deps.app_version.clone(),
                skills: core.deps.skills.clone(),
            });

            if let Ok(mut sessions) = core.sessions.write() {
                sessions.insert(id.clone(), session);
            }

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
            let stored_tasks = core.deps.store.list_tasks(&session_id).await?;
            for mut task in stored_tasks {
                if task.status == pacode_types::TaskStatus::Running {
                    task.status = pacode_types::TaskStatus::Killed;
                    let _ = core.deps.store.upsert_task(&task).await;
                }
            }

            let mut session_tools = core.deps.tools.clone();
            let mcp_tools = core.get_mcp_tools().await;
            for tool in mcp_tools {
                session_tools.register(tool);
            }

            let compaction = core
                .deps
                .store
                .load_compaction(&session_id, &pacode_types::AgentId::main())
                .await?;
            let (summary, upto_seq) = match compaction {
                Some((s, u)) => (Some(s), Some(u)),
                None => (None, None),
            };

            let all_messages = core
                .deps
                .store
                .load_messages(&session_id, &pacode_types::AgentId::main())
                .await?;

            let mut messages_after = Vec::new();
            for row in all_messages {
                if upto_seq.is_none_or(|u| row.seq > u) {
                    messages_after.push((row.seq, Arc::new(row.message)));
                }
            }

            let next_seq = messages_after
                .last()
                .map(|(seq, _)| seq + 1)
                .or(upto_seq.map(|u| u + 1))
                .unwrap_or(0);

            let est_tokens = messages_after
                .iter()
                .map(|(_, m)| m.estimate_tokens())
                .sum::<u32>()
                + summary
                    .as_deref()
                    .map(pacode_types::estimate_tokens)
                    .unwrap_or(0);

            let main_history = crate::agent::History {
                messages: messages_after.iter().map(|(_, m)| m.clone()).collect(),
                next_seq,
                summary: summary.clone(),
                summary_upto: upto_seq.unwrap_or(0),
                estimated_tokens: est_tokens,
            };

            let items = crate::transcript::history_to_items(
                &pacode_types::AgentId::main(),
                &messages_after,
                0,
            );
            let mut transcript_state = crate::transcript::TranscriptState::new(
                core.deps.config.session.history_page as usize * 2,
            );
            for item in items {
                transcript_state.upsert(item);
            }

            let mut agents = BTreeMap::new();
            let main_info = stored_agents
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

            let main_agent = Arc::new(crate::agent::Agent {
                id: pacode_types::AgentId::main(),
                info: RwLock::new(main_info),
                history: std::sync::Mutex::new(main_history),
                injections: crate::inject::InjectionQueue::default(),
                tools: RwLock::new(session_tools.clone()),
                cancel: std::sync::Mutex::new(None),
                turn_lock: tokio::sync::Mutex::new(()),
                transcript: std::sync::Mutex::new(transcript_state),
                prompt: None,
                turns: std::sync::Mutex::new(0),
            });
            agents.insert(pacode_types::AgentId::main(), main_agent);

            for mut sub_info in stored_agents {
                if sub_info.id.is_main() {
                    continue;
                }
                if sub_info.status.is_live() {
                    sub_info.status = pacode_types::AgentStatus::Finished;
                    let _ = core
                        .deps
                        .store
                        .upsert_agent(&session_id, &sub_info, None)
                        .await;
                }
                let sub_agent = Arc::new(crate::agent::Agent {
                    id: sub_info.id.clone(),
                    info: RwLock::new(sub_info.clone()),
                    history: std::sync::Mutex::new(crate::agent::History::default()),
                    injections: crate::inject::InjectionQueue::default(),
                    tools: RwLock::new(session_tools.clone()),
                    cancel: std::sync::Mutex::new(None),
                    turn_lock: tokio::sync::Mutex::new(()),
                    transcript: std::sync::Mutex::new(crate::transcript::TranscriptState::new(
                        core.deps.config.session.history_page as usize * 2,
                    )),
                    prompt: None,
                    turns: std::sync::Mutex::new(0),
                });
                agents.insert(sub_info.id, sub_agent);
            }

            let session = Arc::new(Session {
                id: session_id.clone(),
                meta: RwLock::new(meta),
                agents: RwLock::new(agents),
                plan: RwLock::new(plan),
                usage: RwLock::new(usage),
                permissions: crate::permissions::PermissionState::default(),
                events: crate::transcript::EventSink::new(),
                tasks: core.deps.tasks.clone(),
                store: core.deps.store.clone(),
                config: core.config(),
                providers: core.providers(),
                tools: RwLock::new(session_tools),
                mcp: core.deps.mcp.clone(),
                plugins: core.deps.plugins.clone(),
                app_version: core.deps.app_version.clone(),
                skills: core.deps.skills.clone(),
            });

            if let Ok(mut sessions) = core.sessions.write() {
                sessions.insert(session_id.clone(), session);
            }

            Ok(session_id)
        }
    }
}
