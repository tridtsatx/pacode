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
//!     match agent.take_due(now) {
//!       None → plan reminder due? push it; continue — else break
//!       taken → push render_injections; continue
//!     }
//!     // taken, not yet delivered → agent.restore_due(taken), unchanged
//!   }
//!   run tool calls: consecutive `Tool::runs_concurrently` calls (reads, network, read-only
//!   Safe bash) in parallel (join_all), others sequentially, each:
//!     gate → maybe ask permission (WaitingApproval) → call → cap output → history.push(tool_result) (persist)
//!     emit ItemAdded/ItemUpdated(ToolCall …); cancel → stub tool results ("cancelled") then Interrupted
//!   // point C: urgent interrupt only (cancel token)
//!   // point D
//!   history.extend(render_injections(agent.take_due(now)))
//!   if needs_compaction → compaction::compact
//!   subagent: steps += 1; stop at agents.max_turns with a notice
//! }
//! ```

mod drain;
pub(crate) mod lifecycle;
pub(crate) mod stream;
pub(crate) mod system;
pub(crate) mod tools;

use drain::{DrainOutcome, drain_injections, hide_other_admission_messages};
pub use lifecycle::start_turn;

use std::sync::Arc;

use pacode_types::{AgentStatus, ContentBlock, Event, Message, Role, TurnId, TurnStop, Usage};
use tokio_util::sync::CancellationToken;

use crate::CoreError;
use crate::agent::Agent;
use crate::prompt::DynamicContext;
use crate::session::Session;

/// How [`run_turn`] ended, for its supervisor.
pub(crate) struct TurnEnd {
    pub stop: TurnStop,
    pub usage: Usage,
    /// The turn yielded to a user message before the model chose to end it:
    /// `stop` is `Completed` (nothing failed), but the model never saw the
    /// results of its last step, so what it was shown is not yet answered —
    /// or ignored.
    pub preempted: bool,
}

impl TurnEnd {
    fn new(stop: TurnStop, usage: Usage) -> Self {
        Self {
            stop,
            usage,
            preempted: false,
        }
    }
}

/// Run one turn loop. `start_turn` claims `agent.turn_lock` before spawning
/// this and holds it across the task and its post-block, so a turn always has
/// the agent to itself. `turn_id` is minted by the caller: the supervisor needs
/// it to emit the matching `TurnEnded` — every exit returns the stop + usage
/// and the supervisor emits the event itself, AFTER the cancel token is
/// cleared (an observer reacting to `TurnEnded` must never see the agent
/// still "running"), which also covers this task dying mid-turn.
pub(crate) async fn run_turn(
    session: Arc<Session>,
    agent: Arc<Agent>,
    cancel: CancellationToken,
    turn_id: TurnId,
    admission: Option<String>,
) -> TurnEnd {
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
    // Only this turn's own end at `agents.max_turns` may say so in its report.
    let _ = agent.take_step_capped();
    if cancel.is_cancelled() {
        session.events.emit(Event::TurnStarted {
            agent: agent.id(),
            turn: turn_id,
        });
        session.emit_session_status();
        return TurnEnd::new(
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
    let prompt_cache = system::turn_prompt_cache(&session, &agent, &meta.cwd).await;

    let mut turn_usage = Usage::default();
    let mut last_input_tokens: Option<u64>;
    // The implementer half's own spend. `turn_usage` carries both halves
    // (that is what the user is billed and what `TurnEnded` reports); the
    // fusion trace needs the split, so the two are accumulated apart.
    let mut implementer_usage = Usage::default();
    // Fusion is a property of the turn, resolved once here: `Off` runs the
    // pre-fusion path untouched, `Pending` runs the planner on the first
    // iteration and then becomes `Planned` or `Degraded` for good.
    let mut phase = crate::fusion::start_phase(&session, &agent);
    let mut preempted = false;

    loop {
        if cancel.is_cancelled() {
            *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return TurnEnd::new(lifecycle::handle_interrupted(&session, &agent), turn_usage);
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
                return TurnEnd::new(
                    lifecycle::finish_failed_turn(&session, &agent, error.to_string()),
                    turn_usage,
                );
            }
        }
        // Read once and used both for the request's schemas and for the
        // prompt's `## Tools` inventory, so the two cannot disagree.
        let tools_def = agent
            .tools
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .definitions();
        // Rendered per request against the provider it goes to: the inventory
        // names tools as that provider sends them on the wire, and the planner
        // and the implementer may sit on different providers.
        let render_dynamic =
            |tools: &[pacode_types::ToolDefinition], provider: &dyn pacode_provider::Provider| {
                let plan_guard = session.plan.read().unwrap_or_else(|p| p.into_inner());
                crate::prompt::system_dynamic(&DynamicContext {
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
                    tools,
                    provider,
                })
            };
        let system_static = crate::prompt::system_static().to_string();
        // The fusion seam. Inside the loop, after the injection drain and
        // before the request is built: the planner must see the same history
        // and the same environment block the implementer is about to see, and
        // the plan must land in history before the first implementer request
        // so it is part of the conversation, not a retroactive annotation.
        if let Some(routes) = phase.take_pending() {
            crate::fusion::begin_trace(&agent, &routes);
            match crate::fusion::run_planner(
                &session,
                &agent,
                &cancel,
                &routes,
                &system_static,
                |provider| {
                    render_dynamic(&crate::fusion::planner_tool_definitions(&agent), provider)
                },
            )
            .await
            {
                crate::fusion::PlanOutcome::Planned(artifact) => {
                    // The planner's call was made and paid for whatever
                    // happens to the plan, so it lands in the turn's usage
                    // before the plan is committed.
                    agent.record_turn_usage(artifact.usage);
                    turn_usage += artifact.usage;
                    crate::fusion::record_planner_usage(&agent, artifact.usage);
                    if let Err(error) =
                        crate::fusion::commit_plan(&session, &agent, &artifact).await
                    {
                        return TurnEnd::new(
                            lifecycle::finish_failed_turn(&session, &agent, error.to_string()),
                            turn_usage,
                        );
                    }
                    let implementing = crate::fusion::implementer_activity(&routes.implementer);
                    phase = crate::fusion::Phase::Planned(routes);
                    agent.set_status(AgentStatus::Thinking, Some(implementing));
                    session.events.emit(Event::AgentUpdated(agent.info()));
                }
                crate::fusion::PlanOutcome::Skipped { reason, usage } => {
                    // Nothing was written: the plan is the first thing this
                    // module persists, so a failed planner leaves history as
                    // the user left it and the turn below is the ordinary one.
                    // What it did spend is still the user's money.
                    agent.record_turn_usage(usage);
                    turn_usage += usage;
                    log::warn!(
                        "fusion planner skipped, running the turn on {}: {reason}",
                        agent.info().model
                    );
                    crate::fusion::mark_planner_failed(&agent, usage);
                    // The planner may have labelled the agent before failing;
                    // the turn that follows is an ordinary one and must look
                    // like it.
                    agent.set_status(AgentStatus::Thinking, Some("thinking".to_string()));
                    session.events.emit(Event::AgentUpdated(agent.info()));
                }
            }
        }

        let agent_info = agent.info();
        // The model this request goes to. `Off` and `Degraded` both resolve to
        // the agent's own route, which is what makes the no-fusion path
        // byte-for-byte what it was.
        let model_route = phase.model(&agent_info.model);

        // A context checkpoint, if one is due: a word pushed into the history as
        // an ordinary user message, so compaction can take it away and the
        // settle can tell "lost" from "forgotten". Asked *before* the request is
        // built, so the word is in the very context it is asked about.
        let checkpoint_word = {
            let mut checkpoints = agent
                .checkpoints
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            checkpoints.due(
                session.config.ui.analyze_checkpoint_every,
                agent.compaction_generation(),
            )
        };
        if let Some(word) = checkpoint_word {
            let (seq, message) = agent
                .history
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(pacode_types::Message::user(
                    crate::analyze::checkpoint::instruction(&word),
                ));
            // Persisted with the rest of the conversation: a checkpoint that
            // only existed in memory would vanish on resume, and the resumed
            // session would score differently from the live one.
            let _ = session
                .append_agent_message(agent.as_ref(), seq, &message)
                .await;
        }

        // Re-check the ZDR flag at the resolve point: `SetZdr`/`SetModel`/
        // `SetModelFlag` guards cover the request path, but a turn re-driven
        // by injections or `continue_agent` reaches here without them.
        let provider =
            if session.zdr_active() && !session.model_flags(&model_route.to_string()).zdr_capable {
                Err(pacode_provider::ProviderError::Config(format!(
                    "zdr_blocked: {} is not flagged zdr_capable",
                    model_route
                )))
            } else {
                session.providers.resolve(&model_route).await
            };
        let provider = match provider {
            Ok(p) => p,
            Err(e) => {
                return TurnEnd::new(
                    lifecycle::finish_failed_turn(
                        &session,
                        &agent,
                        crate::fusion::failure_message(&phase, &e.to_string()),
                    ),
                    turn_usage,
                );
            }
        };

        let system_dynamic = render_dynamic(&tools_def, provider.as_ref());
        let history_msgs = agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .for_model();
        // Taken here because this is the last point where the parts the split
        // needs are still owned: the request below moves all of them, and the
        // reported counts only arrive once the model has answered. The cost is
        // a walk over the definitions and the history, both of which are
        // walked again to build the request.
        let attribution = crate::analyze::RequestAttribution::from_request(
            &system_static,
            &system_dynamic,
            &tools_def,
            &history_msgs,
        );
        let req = pacode_provider::CompletionRequest {
            model: model_route.model.clone(),
            system_static,
            system_dynamic,
            messages: history_msgs,
            tools: tools_def,
            effort: Some(agent_info.effort),
            max_output_tokens: None,
        };

        // A provider that accepted the request but never streams back must not
        // hold the turn: cancelling here takes the same path as
        // `outcome.interrupted` below.
        let request_started = std::time::Instant::now();
        let complete_res = tokio::select! {
            _ = cancel.cancelled() => {
                *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
                return TurnEnd::new(
                    lifecycle::handle_interrupted(&session, &agent),
                    turn_usage,
                );
            }
            res = provider.complete(req) => res,
        };

        let stream = match complete_res {
            Ok(s) => s,
            Err(e) => {
                return TurnEnd::new(
                    lifecycle::finish_failed_turn(
                        &session,
                        &agent,
                        crate::fusion::failure_message(&phase, &e.to_string()),
                    ),
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
                    TurnStop::Failed { message } => lifecycle::finish_failed_turn(
                        &session,
                        &agent,
                        crate::fusion::failure_message(&phase, &message),
                    ),
                    TurnStop::Completed | TurnStop::Interrupted => stop,
                };
                return TurnEnd::new(stop, turn_usage);
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
        // The agent's own turn usage, and the session's accounting, are two
        // different questions: this is what the turn cost the budget, the
        // record below is what the request cost in tokens by category. Both
        // happen here, after the provider has reported, and neither replaces
        // the other.
        agent.record_turn_usage(provider_usage);
        session.accounting.record_request(
            attribution.with_usage(&provider_usage),
            Some(crate::analyze::RequestLatency {
                ttft: outcome.first_token_ms.map(std::time::Duration::from_millis),
                total: request_started.elapsed(),
            }),
        );
        turn_usage += outcome.usage;
        implementer_usage += outcome.usage;
        crate::fusion::record_implementer_usage(&agent, implementer_usage);

        if outcome.interrupted {
            *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return TurnEnd::new(lifecycle::handle_interrupted(&session, &agent), turn_usage);
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

        // The checkpoint is answered by a `checkpoint` call in *this* step. A
        // model that saw the word and answered it in prose instead has not
        // demonstrated it reached into the context, so prose does not count:
        // `None` here is an ignored checkpoint, and that is the honest reading.
        let checkpoint_answer = outcome.tool_calls.iter().find_map(|call| {
            (call.name == pacode_tools::builtin::checkpoint::NAME)
                .then(|| call.input.get("word").and_then(|w| w.as_str()))
                .flatten()
                .map(str::to_string)
        });
        {
            let mut checkpoints = agent
                .checkpoints
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if checkpoints.pending_word().is_some() {
                checkpoints.settle(checkpoint_answer.as_deref(), agent.compaction_generation());
                // Published every turn, not only when a checkpoint was asked:
                // the page reads the scorecard when it is opened, and a
                // scorecard that only updates on a checkpoint is a stale one.
                session
                    .accounting
                    .record_context_quality(checkpoints.report());
            }
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
            return TurnEnd::new(
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
            && let Some(title_guard) = crate::naming::begin_title(&session.id)
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
                let _title_guard = title_guard;
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
                Ok(DrainOutcome::DeferredAdmission) => break,
                // The model chose to stop: an open plan may keep it going.
                Ok(DrainOutcome::Idle) => {
                    if crate::plan_reminder::remind(&session, &agent).await {
                        continue;
                    }
                    break;
                }
                Ok(DrainOutcome::Continue) => continue,
                Err(error) => {
                    return TurnEnd::new(
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
            return TurnEnd::new(lifecycle::handle_interrupted(&session, &agent), turn_usage);
        }

        match drain_injections(&session, &agent, admission.as_deref(), true).await {
            // The user's message takes over before the model has seen the
            // results of this step: the turn ends, but not on its own terms.
            Ok(DrainOutcome::DeferredAdmission) => {
                preempted = true;
                break;
            }
            Ok(DrainOutcome::Idle | DrainOutcome::Continue) => {}
            Err(error) => {
                return TurnEnd::new(
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
        // The window of the model that is actually carrying this turn. Off
        // fusion that is the agent's own model, as before.
        let model_info = provider.model_info(&model_route.model);
        let context_window = crate::compaction::resolve_context_window(
            Some(&model_info),
            session.config.context.default_context_window,
        );
        let threshold = session.config.context.compaction_threshold;
        if crate::compaction::needs_compaction(last_input_tokens, context_window, threshold)
            && let Err(error) =
                crate::compaction::compact_auto(&session, &agent, last_input_tokens).await
        {
            log::warn!(
                "compaction failed: session={} agent={}: {error}",
                session.id,
                agent.id()
            );
        }

        // The only stop a subagent caught in a tool loop has: past
        // `agents.max_turns` steps its turn ends like a completed one, and the
        // report it ends with says why (`lifecycle`).
        let max_turns = session.config.agents.max_turns;
        if !agent.id().is_main() && agent.count_step() >= max_turns {
            log::warn!(
                "subagent reached agents.max_turns ({max_turns}): session={} agent={}",
                session.id,
                agent.id()
            );
            session.emit_notice(
                &agent,
                pacode_types::ToastLevel::Warn,
                format!("Subagent stopped: reached the maximum turns limit ({max_turns})"),
            );
            agent.mark_step_capped();
            break;
        }
    }

    // The turn finished on the implementer: the trace can say so. A turn that
    // broke out early (deferred admission) leaves the phase where it stopped,
    // which is the truth about what the client was shown.
    if phase.planned() {
        crate::fusion::complete_trace(&agent, implementer_usage);
    }

    agent.clear_error();
    // A subagent's terminal status is not this loop's to publish: the
    // post-block first decides whether the agent owes another turn (a message
    // or an admission that landed on the last step) and stamps `Finished`
    // only when it does not — a `Finished` published here would flash on the
    // card, and as a finished toast, for an agent that is about to go on.
    if agent.id().is_main() {
        agent.set_status(AgentStatus::Idle, None);
        session.events.emit(Event::AgentUpdated(agent.info()));
    }

    *agent.prompt_cache.lock().unwrap_or_else(|p| p.into_inner()) = None;

    TurnEnd {
        stop: TurnStop::Completed,
        usage: turn_usage,
        preempted,
    }
}

fn compaction_input_tokens(last_input_tokens: Option<u64>, estimated_tokens: u32) -> u32 {
    last_input_tokens
        .map(|tokens| tokens.min(u32::MAX as u64) as u32)
        .unwrap_or(estimated_tokens)
}

pub(crate) async fn persist_history_message(
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
