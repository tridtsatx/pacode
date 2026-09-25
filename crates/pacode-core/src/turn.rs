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
//! }
//! ```

pub(crate) mod lifecycle;
pub(crate) mod stream;
pub(crate) mod tools;

pub use lifecycle::start_turn;

use std::sync::Arc;

use pacode_types::{AgentStatus, ContentBlock, Event, Message, Role, TurnId, TurnStop, Usage};
use tokio_util::sync::CancellationToken;

use crate::CoreError;
use crate::agent::Agent;
use crate::prompt::DynamicContext;
use crate::session::Session;

/// Run one turn loop. `start_turn` claims `agent.turn_lock` before spawning
/// this and holds it across the task and its post-block, so a turn always has
/// the agent to itself. `turn_id` is minted by the caller: the supervisor needs
/// it to emit the matching `TurnEnded` — every exit returns the stop + usage
/// and the supervisor emits the event itself, AFTER the cancel token is
/// cleared (an observer reacting to `TurnEnded` must never see the agent
/// still "running"), which also covers this task dying mid-turn.
pub async fn run_turn(
    session: Arc<Session>,
    agent: Arc<Agent>,
    cancel: CancellationToken,
    turn_id: TurnId,
    admission: Option<String>,
) -> (TurnStop, Usage) {
    // `start_turn` holds `agent.turn_lock` for the whole call — a bare
    // `run_turn` would race that turn's cleanup, so fail loudly in debug.
    debug_assert!(agent.turn_lock.try_lock().is_err());

    log::info!(
        "turn start: session={} agent={} turn={}",
        session.id,
        agent.id(),
        turn_id
    );
    if let Some(admission) = admission.as_deref() {
        agent.mark_admission_progressed(admission, &turn_id);
    }
    if cancel.is_cancelled() {
        session.events.emit(Event::TurnStarted {
            agent: agent.id(),
            turn: turn_id,
        });
        session.emit_session_status();
        return (
            lifecycle::handle_interrupted(&session, &agent),
            Usage::default(),
        );
    }
    let _ = session
        .plugins
        .run_hooks(&pacode_plugin::HookEvent::TurnStart)
        .await;

    // `session_start` fires lazily on the first turn: the session-open path
    // lives in core/open.rs, so this is the closest owned seam. Deduped per
    // session id inside the hooks module.
    if agent.id().is_main() {
        crate::hooks::session_start(&session, &agent);
    }

    if !agent.stop_requested() {
        agent.clear_error();
        agent.set_status(AgentStatus::Thinking, Some("thinking".to_string()));
    }
    session.events.emit(Event::TurnStarted {
        agent: agent.id(),
        turn: turn_id.clone(),
    });
    session.emit_session_status();
    if !agent.stop_requested() {
        session.events.emit(Event::AgentUpdated(agent.info()));
    }

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
    let mut last_input_tokens: Option<u64>;

    loop {
        if cancel.is_cancelled() {
            *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return (lifecycle::handle_interrupted(&session, &agent), turn_usage);
        }

        // Fold queued injections into history BEFORE building the request: a
        // turn started by an injection (e.g. `AgentFinished` re-driving the
        // parent) must see it in the first call — otherwise the provider gets
        // a stale (or even empty) message list and the injection only lands
        // after a wasted round-trip.
        match drain_injections(&session, &agent, admission.as_deref(), false).await {
            Ok(DrainOutcome::DeferredAdmission) => break,
            Ok(DrainOutcome::Idle | DrainOutcome::Continue) => {}
            Err(error) => {
                return (
                    lifecycle::finish_failed_turn(&session, &agent, error.to_string()),
                    turn_usage,
                );
            }
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

        // Re-check the ZDR flag at the resolve point: `SetZdr`/`SetModel`/
        // `SetModelFlag` guards cover the request path, but a turn re-driven
        // by injections or `continue_agent` reaches here without them.
        let provider = if session.zdr_active()
            && !session
                .model_flags(&agent_info.model.to_string())
                .zdr_capable
        {
            Err(pacode_provider::ProviderError::Config(format!(
                "zdr_blocked: {} is not flagged zdr_capable",
                agent_info.model
            )))
        } else {
            session.providers.resolve(&agent_info.model).await
        };
        let provider = match provider {
            Ok(p) => p,
            Err(e) => {
                return (
                    lifecycle::finish_failed_turn(&session, &agent, e.to_string()),
                    turn_usage,
                );
            }
        };

        // A provider that accepted the request but never streams back must not
        // hold the turn: cancelling here takes the same path as
        // `outcome.interrupted` below.
        let complete_res = tokio::select! {
            _ = cancel.cancelled() => {
                *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
                return (
                    lifecycle::handle_interrupted(&session, &agent),
                    turn_usage,
                );
            }
            res = provider.complete(req) => res,
        };

        let stream = match complete_res {
            Ok(s) => s,
            Err(e) => {
                return (
                    lifecycle::finish_failed_turn(&session, &agent, e.to_string()),
                    turn_usage,
                );
            }
        };

        let outcome = match stream::consume_stream(&session, &agent, &cancel, stream).await {
            Ok(out) => out,
            Err(stop) => {
                // `consume_stream` yields only `Failed` today; a `Failed`
                // still wants the notice + status exit, the rest unwinds as-is.
                let stop = match stop {
                    TurnStop::Failed { message } => {
                        lifecycle::finish_failed_turn(&session, &agent, message)
                    }
                    TurnStop::Completed | TurnStop::Interrupted => stop,
                };
                return (stop, turn_usage);
            }
        };

        let observed_usage = agent.take_stream_usage();
        let provider_usage = if observed_usage.is_empty() {
            outcome.usage
        } else {
            observed_usage
        };
        last_input_tokens =
            (provider_usage.input_tokens > 0).then_some(provider_usage.input_tokens);
        agent.record_turn_usage(provider_usage);
        turn_usage += outcome.usage;

        if outcome.interrupted {
            *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return (lifecycle::handle_interrupted(&session, &agent), turn_usage);
        }

        let mut content_blocks = Vec::new();
        if !outcome.reasoning.is_empty() || outcome.reasoning_signature.is_some() {
            content_blocks.push(ContentBlock::Reasoning {
                text: outcome.reasoning,
                signature: outcome.reasoning_signature,
            });
        }
        if !outcome.text.is_empty() {
            content_blocks.push(ContentBlock::Text {
                text: outcome.text.clone(),
            });
        }

        for call in &outcome.tool_calls {
            content_blocks.push(ContentBlock::ToolUse {
                id: call.call_id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            });
        }

        if let Err(error) = persist_history_message(
            &session,
            &agent,
            Message::new(Role::Assistant, content_blocks),
        )
        .await
        {
            return (
                lifecycle::finish_failed_turn(&session, &agent, error.to_string()),
                turn_usage,
            );
        }

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
            match drain_injections(&session, &agent, admission.as_deref(), true).await {
                Ok(DrainOutcome::DeferredAdmission | DrainOutcome::Idle) => break,
                Ok(DrainOutcome::Continue) => continue,
                Err(error) => {
                    return (
                        lifecycle::finish_failed_turn(&session, &agent, error.to_string()),
                        turn_usage,
                    );
                }
            }
        }

        let tool_interrupted =
            tools::execute_tool_calls(&session, &agent, &cancel, outcome.tool_calls).await;
        if tool_interrupted || cancel.is_cancelled() {
            *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return (lifecycle::handle_interrupted(&session, &agent), turn_usage);
        }

        match drain_injections(&session, &agent, admission.as_deref(), true).await {
            Ok(DrainOutcome::DeferredAdmission) => break,
            Ok(DrainOutcome::Idle | DrainOutcome::Continue) => {}
            Err(error) => {
                return (
                    lifecycle::finish_failed_turn(&session, &agent, error.to_string()),
                    turn_usage,
                );
            }
        }

        let est_tokens = agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .estimated_tokens;
        // Spec §6.4: trigger based on last observed usage.input_tokens (or est_tokens if none)
        // against the active model's context window.
        let last_input_tokens = compaction_input_tokens(last_input_tokens, est_tokens);
        let model_info = provider.model_info(&agent_info.model.model);
        let context_window = crate::compaction::resolve_context_window(
            Some(&model_info),
            session.config.context.default_context_window,
        );
        let threshold = session.config.context.compaction_threshold;
        if crate::compaction::needs_compaction(last_input_tokens, context_window, threshold)
            && let Err(error) = crate::compaction::compact(&session, &agent).await
        {
            log::warn!(
                "compaction failed: session={} agent={}: {error}",
                session.id,
                agent.id()
            );
        }
    }

    let final_status = if agent.id().is_main() {
        AgentStatus::Idle
    } else {
        AgentStatus::Finished
    };
    if matches!(final_status, AgentStatus::Idle | AgentStatus::Finished) {
        agent.clear_error();
    }
    agent.set_status(final_status, None);
    session.events.emit(Event::AgentUpdated(agent.info()));

    *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;

    (TurnStop::Completed, turn_usage)
}

fn compaction_input_tokens(last_input_tokens: Option<u64>, estimated_tokens: u32) -> u32 {
    last_input_tokens
        .map(|tokens| tokens.min(u32::MAX as u64) as u32)
        .unwrap_or(estimated_tokens)
}

enum DrainOutcome {
    Idle,
    Continue,
    DeferredAdmission,
}

async fn drain_injections(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    admission: Option<&str>,
    defer_admissions: bool,
) -> Result<DrainOutcome, CoreError> {
    let drained = agent.injections.drain();
    if defer_admissions
        && drained.iter().any(|item| {
            matches!(
                item,
                crate::inject::Injection::UserSteer {
                    admission: Some(candidate)
                } if Some(candidate.as_str()) != admission
            )
        })
    {
        agent.injections.restore(drained);
        return Ok(DrainOutcome::DeferredAdmission);
    }

    let mut deferred = Vec::new();
    let mut active = Vec::new();
    for item in drained {
        if matches!(
            &item,
            crate::inject::Injection::UserSteer {
                admission: Some(candidate)
            } if Some(candidate.as_str()) != admission
        ) {
            deferred.push(item);
        } else {
            active.push(item);
        }
    }

    if let Err(error) = hide_other_admission_messages(session, agent, admission).await {
        agent.injections.restore(deferred);
        return Err(error);
    }

    if active.is_empty() {
        agent.injections.restore(deferred);
        return Ok(DrainOutcome::Idle);
    }

    let has_user_steer = active
        .iter()
        .any(|item| matches!(item, crate::inject::Injection::UserSteer { .. }));
    let renderable = active
        .into_iter()
        .filter(|item| !matches!(item, crate::inject::Injection::UserSteer { .. }))
        .collect::<Vec<_>>();
    if renderable.is_empty() {
        agent.injections.restore(deferred);
        return Ok(if has_user_steer {
            DrainOutcome::Continue
        } else {
            DrainOutcome::Idle
        });
    }

    let injection_msg =
        crate::inject::render_injections(&renderable, session.config.context.injection_cap_chars);
    if let Err(error) = persist_history_message(session, agent, injection_msg).await {
        agent.injections.restore(deferred);
        return Err(error);
    }
    agent.injections.restore(deferred);
    Ok(DrainOutcome::Continue)
}

async fn hide_other_admission_messages(
    session: &Session,
    agent: &Agent,
    current_admission: Option<&str>,
) -> Result<(), CoreError> {
    // Both queries are keyed by (session, agent): this runs on every injection
    // drain, several times per model step, and the agent's own history can be
    // tens of thousands of rows long.
    let mut message_seqs = session
        .store
        .cancelled_message_seqs(&session.id, &agent.id())
        .await?;
    for record in session
        .store
        .load_unresolved_message_admissions(&session.id, &agent.id())
        .await?
    {
        if current_admission == Some(record.message_id.as_str()) {
            continue;
        }
        if let Some(seq) = record.message_seq {
            message_seqs.insert(seq);
        }
    }
    if message_seqs.is_empty() {
        return Ok(());
    }

    let mut history = agent.history.lock().unwrap_or_else(|p| p.into_inner());
    let mut index = 0;
    while index < history.message_seqs.len() {
        if message_seqs.contains(&history.message_seqs[index]) {
            history.message_seqs.remove(index);
            history.messages.remove(index);
        } else {
            index += 1;
        }
    }
    history.estimated_tokens = history
        .messages
        .iter()
        .map(|message| message.estimate_tokens())
        .sum();
    Ok(())
}

async fn persist_history_message(
    session: &Session,
    agent: &Agent,
    message: Message,
) -> Result<(), CoreError> {
    let reservation = agent
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .reserve(message);
    if let Err(error) = session
        .append_agent_message(agent, reservation.seq(), reservation.message())
        .await
    {
        agent
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .rollback_reserved(&reservation);
        return Err(error);
    }
    agent
        .history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .commit_reserved(&reservation);
    Ok(())
}
