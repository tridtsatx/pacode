//! The turn loop (spec §6.2).
//!
//! ```text
//! loop {
//!   build CompletionRequest (system_static, system_dynamic, tools, history.for_model())
//!   stream = provider.complete(req)            // retries inside the provider
//!   consume: TextDelta → coalesce → Event::TextDelta; ReasoningDelta likewise;
//!            ToolCallStart/ArgsDelta → assemble ToolUse blocks; Usage → record;
//!            MessageEnd → stop reason. Cancel token → abort stream, TurnStop::Interrupted.
//!   history.push(assistant message)  (persist)  // append-only
//!   emit ItemUpdated(assistant complete)
//!   if no tool calls {
//!     // point B
//!     match injections.drain() { empty → break; items → push render_injections; continue }
//!   }
//!   run tool calls: ReadOnly/Network in parallel (join_all), others sequentially, each:
//!     gate → maybe ask permission (WaitingApproval) → call → cap output → history.push(tool_result) (persist)
//!     emit ItemAdded/ItemUpdated(ToolCall …); cancel → stub tool results ("cancelled") then Interrupted
//!   // point C: urgent interrupt only (cancel token)
//!   // point D
//!   history.extend(render_injections(injections.drain()))
//!   if needs_compaction → compaction::compact
//!   subagent: turns += 1; stop at agents.max_turns with a notice
//! }
//! ```

pub(crate) mod lifecycle;
pub(crate) mod stream;
pub(crate) mod tools;

pub(crate) use lifecycle::emit_failure_notice;
pub use lifecycle::start_turn;

use std::sync::Arc;

use pacode_types::{
    AgentStatus, ContentBlock, Event, Message, Role, TranscriptItem, TranscriptKind, TurnId,
    TurnStop, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::prompt::DynamicContext;
use crate::session::Session;

pub async fn run_turn(
    session: Arc<Session>,
    agent: Arc<Agent>,
    cancel: CancellationToken,
) -> TurnStop {
    let _turn_guard = match agent.turn_lock.try_lock() {
        Ok(g) => g,
        Err(_) => {
            return TurnStop::Failed {
                message: "a turn is already running on this agent".to_string(),
            };
        }
    };

    let turn_id = TurnId::generate();
    let turn_start_instant = std::time::Instant::now();
    log::info!(
        "turn start: session={} agent={} turn={}",
        session.id,
        agent.id(),
        turn_id
    );
    let _ = session
        .plugins
        .run_hooks(&pacode_plugin::HookEvent::TurnStart)
        .await;

    session.events.emit(Event::TurnStarted {
        agent: agent.id(),
        turn: turn_id.clone(),
    });
    agent.set_status(AgentStatus::Thinking, Some("thinking".to_string()));
    session.events.emit(Event::AgentUpdated(agent.info()));

    // Turn start is the explicit invalidation point for prompt caching
    let meta = session.meta();
    let home_config = pacode_config::Paths::discover()
        .config_file
        .parent()
        .map(|d| d.to_path_buf());
    let prompt_cache = crate::prompt::TurnPromptCache::new(
        &meta.cwd,
        home_config.as_deref(),
        session.config.context.instructions_cap_chars,
        session.config.context.memory_cap_chars,
    );
    *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = Some(prompt_cache.clone());

    let mut turn_usage = Usage::default();

    loop {
        if cancel.is_cancelled() {
            *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return lifecycle::handle_interrupted(
                &session,
                &agent,
                &turn_id,
                turn_start_instant,
                turn_usage,
            )
            .await;
        }

        let system_dynamic = {
            let plan_guard = session.plan.read().unwrap_or_else(|p| p.into_inner());
            let dyn_ctx = DynamicContext {
                cwd: &meta.cwd,
                git_branch: prompt_cache.git_branch.as_deref(),
                date: &prompt_cache.date,
                mode: meta.mode,
                plan: &plan_guard,
                instructions: prompt_cache.instructions.as_deref(),
                is_subagent: !agent.id().is_main(),
                skills: session.skills.skills(),
                skills_enabled: session.config.skills.enabled,
                max_listed_skills: session.config.skills.max_listed,
            };
            crate::prompt::system_dynamic(&dyn_ctx)
        };
        let system_static = crate::prompt::system_static().to_string();

        let agent_info = agent.info();
        let history_msgs = agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .for_model();
        let tools_def = agent
            .tools
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .definitions();

        let req = pacode_provider::CompletionRequest {
            model: agent_info.model.model.clone(),
            system_static,
            system_dynamic,
            messages: history_msgs,
            tools: tools_def,
            effort: Some(agent_info.effort),
            max_output_tokens: None,
        };

        let provider = match session.providers.resolve(&agent_info.model) {
            Ok(p) => p,
            Err(e) => {
                emit_failure_notice(&session, &agent, &e.to_string());
                let stop = TurnStop::Failed {
                    message: e.to_string(),
                };
                let duration_ms = turn_start_instant.elapsed().as_millis() as u64;
                let output_tokens = turn_usage.output_tokens;
                log::warn!(
                    "turn end: session={} agent={} turn={} stop=Failed duration={}ms error={e}",
                    session.id,
                    agent.id(),
                    turn_id,
                    duration_ms
                );
                let _ = session
                    .plugins
                    .run_hooks(&pacode_plugin::HookEvent::TurnEnd {
                        duration_ms,
                        output_tokens,
                    })
                    .await;
                session.events.emit(Event::TurnEnded {
                    agent: agent.id(),
                    turn: turn_id,
                    usage: Some(turn_usage),
                    stop: stop.clone(),
                });
                return stop;
            }
        };

        let stream = match provider.complete(req).await {
            Ok(s) => s,
            Err(e) => {
                emit_failure_notice(&session, &agent, &e.to_string());
                let stop = TurnStop::Failed {
                    message: e.to_string(),
                };
                let duration_ms = turn_start_instant.elapsed().as_millis() as u64;
                let output_tokens = turn_usage.output_tokens;
                log::warn!(
                    "turn end: session={} agent={} turn={} stop=Failed duration={}ms error={e}",
                    session.id,
                    agent.id(),
                    turn_id,
                    duration_ms
                );
                let _ = session
                    .plugins
                    .run_hooks(&pacode_plugin::HookEvent::TurnEnd {
                        duration_ms,
                        output_tokens,
                    })
                    .await;
                session.events.emit(Event::TurnEnded {
                    agent: agent.id(),
                    turn: turn_id,
                    usage: Some(turn_usage),
                    stop: stop.clone(),
                });
                return stop;
            }
        };

        let outcome = match stream::consume_stream(&session, &agent, &cancel, stream).await {
            Ok(out) => out,
            Err(stop) => {
                if let TurnStop::Failed { message } = &stop {
                    emit_failure_notice(&session, &agent, message);
                }
                let duration_ms = turn_start_instant.elapsed().as_millis() as u64;
                let output_tokens = turn_usage.output_tokens;
                let _ = session
                    .plugins
                    .run_hooks(&pacode_plugin::HookEvent::TurnEnd {
                        duration_ms,
                        output_tokens,
                    })
                    .await;
                session.events.emit(Event::TurnEnded {
                    agent: agent.id(),
                    turn: turn_id,
                    usage: Some(turn_usage),
                    stop: stop.clone(),
                });
                return stop;
            }
        };

        turn_usage = outcome.usage;

        if outcome.interrupted {
            *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return lifecycle::handle_interrupted(
                &session,
                &agent,
                &turn_id,
                turn_start_instant,
                turn_usage,
            )
            .await;
        }

        let mut content_blocks = Vec::new();
        if !outcome.reasoning.is_empty() {
            content_blocks.push(ContentBlock::Reasoning {
                text: outcome.reasoning,
                signature: None,
            });
        }
        if !outcome.text.is_empty() {
            content_blocks.push(ContentBlock::Text {
                text: outcome.text.clone(),
            });
        }

        for (call_id, name, input, _) in &outcome.tool_calls {
            content_blocks.push(ContentBlock::ToolUse {
                id: call_id.clone(),
                name: name.clone(),
                input: input.clone(),
            });
        }

        let assistant_msg = Message::new(Role::Assistant, content_blocks);
        let (msg_seq, arc_msg) = agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(assistant_msg.clone());
        let _ = session
            .store
            .append_message(&session.id, &agent.id(), msg_seq, &arc_msg)
            .await;

        let _ = session
            .plugins
            .run_hooks(&pacode_plugin::HookEvent::OnMessage {
                role: "assistant".to_string(),
                text: outcome.text.clone(),
            })
            .await;

        if agent.id().is_main()
            && session.meta().name.is_none()
            && !outcome.text.is_empty()
            && let Some(first_prompt) = session.meta().first_prompt.clone()
        {
            let session_clone = session.clone();
            let provider_clone = provider.clone();
            let model_name = session
                .config
                .session
                .title_model
                .clone()
                .unwrap_or_else(|| agent_info.model.model.clone());
            let first_ans = outcome.text.clone();
            tokio::spawn(async move {
                if let Some(title) = crate::naming::generate_title(
                    provider_clone,
                    &model_name,
                    &first_prompt,
                    &first_ans,
                )
                .await
                {
                    {
                        let mut m = session_clone
                            .meta
                            .write()
                            .unwrap_or_else(|p| p.into_inner());
                        m.name = Some(title);
                    }
                    session_clone.touch();
                }
            });
        }

        if outcome.tool_calls.is_empty() {
            let drained = agent.injections.drain();
            if drained.is_empty() {
                break;
            } else {
                let injection_msg = crate::inject::render_injections(
                    &drained,
                    session.config.context.injection_cap_chars,
                );
                let (inj_seq, arc_inj) = agent
                    .history
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(injection_msg);
                let _ = session
                    .store
                    .append_message(&session.id, &agent.id(), inj_seq, &arc_inj)
                    .await;
                continue;
            }
        }

        let tool_interrupted =
            tools::execute_tool_calls(&session, &agent, &cancel, outcome.tool_calls).await;
        if tool_interrupted || cancel.is_cancelled() {
            *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return lifecycle::handle_interrupted(
                &session,
                &agent,
                &turn_id,
                turn_start_instant,
                turn_usage,
            )
            .await;
        }

        let drained = agent.injections.drain();
        if !drained.is_empty() {
            let injection_msg = crate::inject::render_injections(
                &drained,
                session.config.context.injection_cap_chars,
            );
            let (inj_seq, arc_inj) = agent
                .history
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(injection_msg);
            let _ = session
                .store
                .append_message(&session.id, &agent.id(), inj_seq, &arc_inj)
                .await;
        }

        let est_tokens = agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .estimated_tokens;
        // Spec §6.4: trigger based on last observed usage.input_tokens (or est_tokens if none)
        // against the active model's context window.
        let last_input_tokens = if turn_usage.input_tokens > 0 {
            turn_usage.input_tokens as u32
        } else {
            est_tokens
        };
        let model_info = provider.model_info(&agent_info.model.model);
        let context_window = crate::compaction::resolve_context_window(
            Some(&model_info),
            session.config.context.default_context_window,
        );
        let threshold = session.config.context.compaction_threshold;
        if crate::compaction::needs_compaction(last_input_tokens, context_window, threshold) {
            let _ = crate::compaction::compact(&session, &agent).await;
        }

        if !agent.id().is_main() {
            let mut turns_guard = agent.turns.lock().unwrap_or_else(|p| p.into_inner());
            *turns_guard += 1;
            if *turns_guard >= session.config.agents.max_turns {
                let notice_item = TranscriptItem {
                    seq: agent
                        .transcript
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .next_seq(),
                    agent: agent.id(),
                    ts_ms: pacode_types::now_ms(),
                    kind: TranscriptKind::Notice {
                        level: pacode_types::ToastLevel::Warn,
                        text: format!(
                            "Subagent stopped: reached maximum turns limit ({})",
                            session.config.agents.max_turns
                        ),
                    },
                };
                agent
                    .transcript
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .upsert(notice_item.clone());
                session.events.emit(Event::ItemAdded(notice_item));
                break;
            }
        }
    }

    let final_status = if agent.id().is_main() {
        AgentStatus::Idle
    } else {
        AgentStatus::Finished
    };
    agent.set_status(final_status, None);
    session.events.emit(Event::AgentUpdated(agent.info()));

    let context_tokens = agent
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .estimated_tokens;
    session.record_usage(&agent.id(), turn_usage, context_tokens);

    let duration_ms = turn_start_instant.elapsed().as_millis() as u64;
    let output_tokens = turn_usage.output_tokens;
    log::info!(
        "turn end: session={} agent={} turn={} stop=Completed duration={}ms output_tokens={output_tokens}",
        session.id,
        agent.id(),
        turn_id,
        duration_ms
    );
    let _ = session
        .plugins
        .run_hooks(&pacode_plugin::HookEvent::TurnEnd {
            duration_ms,
            output_tokens,
        })
        .await;

    session.events.emit(Event::TurnEnded {
        agent: agent.id(),
        turn: turn_id,
        usage: Some(turn_usage),
        stop: TurnStop::Completed,
    });

    *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;

    TurnStop::Completed
}
