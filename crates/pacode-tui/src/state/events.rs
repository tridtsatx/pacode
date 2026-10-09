//! Folding daemon events into transcript, rail, panel, and toasts.

use std::time::Instant;

use pacode_types::{AgentId, Event, PermissionDecision, ToastLevel, TranscriptKind};

use crate::state::session::TurnEndMatch;
use crate::state::stats;
use crate::state::transcript::{CellKind, PermExplain, Transcript};
use crate::state::{AppState, Focus, PanelTarget};

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;

pub fn apply_event(state: &mut AppState, seq: u64, event: Event, now: Instant) {
    match event {
        Event::SessionUpdated(meta) => {
            // Only what this session changed moves the global defaults; a
            // title touch or another session's update writes nothing. The
            // write itself is the event loop's, off the UI thread.
            if let Some(old) = state.meta.as_ref() {
                let mut delta = crate::state::PrefsDelta::between(old, &meta);
                // The daemon accepted a route switch: both ends are recent.
                // Recorded here, not on the request, so a refused switch never
                // lands in prefs.
                if delta.model.is_some() {
                    let old = old.model.to_string();
                    state.push_recent_model(&old);
                    state.push_recent_model(&meta.model.to_string());
                    delta.recent_models = Some(state.recent_models.clone());
                }
                state.pending_prefs.merge(delta);
            }
            let header = crate::state::transcript::HeaderInfo {
                version: state.app_version.clone(),
                day: crate::ui::phrases::day_index(pacode_types::time::now_ms()),
                mascot: state.mascot,
                truecolor: state.truecolor,
            };
            state.transcript.set_header(header);
            state.meta = Some(meta);
        }
        Event::TurnStarted { agent, turn } => {
            if agent.is_main() {
                if state.session.observe_turn_started_for(Some(&turn)) {
                    state.turn_started_at = Some(now);
                    state.rail.update_idle(true, now);
                }
            } else {
                state.rail.begin_agent_turn(&agent, &turn);
            }
        }
        Event::TurnEnded {
            agent,
            turn,
            usage,
            stop,
        } => {
            let turn_match = if agent.is_main() {
                state.session.observe_turn_end(&turn)
            } else {
                state
                    .rail
                    .finish_agent_turn(&agent, &turn, &stop, pacode_types::time::now_ms())
            };
            if let pacode_types::TurnStop::Failed { message } = &stop {
                state.push_toast(
                    ToastLevel::Error,
                    "turn failed".to_string(),
                    Some(message.clone()),
                    now,
                );
            }
            if turn_match != TurnEndMatch::Stale {
                if agent.is_main() {
                    state.transcript.flush_stream();
                    if let Some(ref u) = usage
                        && u.output_tokens > 0
                    {
                        let duration_ms = state
                            .turn_started_at
                            .map(|t| now.saturating_duration_since(t).as_millis() as u64)
                            .unwrap_or_else(|| {
                                let last_ts = state
                                    .transcript
                                    .cells
                                    .iter()
                                    .rev()
                                    .find_map(|c| {
                                        if matches!(
                                            c.kind,
                                            CellKind::Item(TranscriptKind::Assistant { .. })
                                        ) {
                                            Some(c.ts_ms)
                                        } else {
                                            None
                                        }
                                    })
                                    .unwrap_or(0);
                                pacode_types::time::now_ms().saturating_sub(last_ts)
                            });
                        if let Some(stats) = compute_turn_stats(duration_ms, u) {
                            attach_turn_stats(&mut state.transcript, stats);
                        }
                    }
                    state.turn_started_at = None;
                } else {
                    if let Some(transcript) = transcript_for_agent(state, &agent) {
                        transcript.flush_stream();
                        if let Some(ref u) = usage
                            && u.output_tokens > 0
                        {
                            let last_ts = transcript
                                .cells
                                .iter()
                                .rev()
                                .find_map(|c| {
                                    if matches!(
                                        c.kind,
                                        CellKind::Item(TranscriptKind::Assistant { .. })
                                    ) {
                                        Some(c.ts_ms)
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(0);
                            let duration_ms = pacode_types::time::now_ms().saturating_sub(last_ts);
                            if let Some(stats) = compute_turn_stats(duration_ms, u) {
                                attach_turn_stats(transcript, stats);
                            }
                        }
                    }
                }
            }
        }
        Event::ItemAdded(item) => {
            if let TranscriptKind::ToolCall { name, title, .. } = &item.kind {
                let arg = crate::state::files::title_path_arg(name, title);
                let input = serde_json::json!({ "path": arg });
                state.files.observe_tool_item(name, &input, item.ts_ms);
            }
            let agent = item.agent.clone();
            if let Some(transcript) = transcript_for_agent(state, &agent) {
                transcript.upsert(item, now);
            }
        }
        Event::ToolOutput {
            agent,
            item_seq,
            text,
        } => {
            // The cell grows while the process runs. `push_tool_output` refuses
            // anything that is not a running tool row, so a chunk that arrives
            // after the call finished is dropped rather than appended to a
            // result the model has already read. Item seqs are per agent, so
            // the chunk only goes to its own agent's transcript.
            if let Some(transcript) = transcript_for_agent(state, &agent) {
                transcript.push_tool_output(item_seq, &text);
                state.dirty = true;
            }
        }
        Event::ItemUpdated(item) => {
            // The finished call's `DiffStat` lands here — fold it into the
            // row `ItemAdded` created (§5.5's `+A −R` column).
            if let TranscriptKind::ToolCall {
                name,
                title,
                call_id,
                diff: Some(diff),
                ..
            } = &item.kind
            {
                let arg = crate::state::files::title_path_arg(name, title);
                let input = serde_json::json!({ "path": arg });
                state
                    .files
                    .apply_diff(name, &input, call_id, *diff, item.ts_ms);
            }
            let agent = item.agent.clone();
            if let Some(transcript) = transcript_for_agent(state, &agent) {
                transcript.upsert(item, now);
            }
        }
        Event::TextDelta {
            agent,
            item_seq,
            text,
        } => {
            if let Some(transcript) = transcript_for_agent(state, &agent) {
                transcript.push_delta(item_seq, &text, false, now);
            }
        }
        Event::ReasoningDelta {
            agent,
            item_seq,
            text,
        } => {
            if let Some(transcript) = transcript_for_agent(state, &agent) {
                transcript.push_delta(item_seq, &text, true, now);
            }
        }
        Event::PermissionRequested(req) => {
            let ts_ms = req.created_at_ms;
            // The daemon also records the request as a transcript item — the
            // upsert keys on the permission id so both land on one prompt.
            state.transcript.upsert_permission(req, ts_ms, seq);
        }
        Event::PermissionResolved {
            permission,
            decision,
            reason,
        } => {
            for cell in &mut state.transcript.cells {
                if let CellKind::Item(TranscriptKind::Permission(ref req)) = cell.kind
                    && req.id == permission
                {
                    let (level, text) = match decision {
                        PermissionDecision::AllowOnce => {
                            (ToastLevel::Success, format!("Allowed: {}", req.title))
                        }
                        PermissionDecision::AllowSession => (
                            ToastLevel::Success,
                            format!("Allowed for session: {}", req.title),
                        ),
                        PermissionDecision::Deny => (
                            ToastLevel::Warn,
                            // The title stays so the row still says what was
                            // denied; the reason explains who/what denied it
                            // ("interrupted", a client note, …).
                            match &reason {
                                Some(reason) => format!("Denied: {} — {reason}", req.title),
                                None => format!("Denied: {}", req.title),
                            },
                        ),
                    };
                    cell.kind = CellKind::Item(TranscriptKind::Notice { level, text });
                    cell.perm_explain = None;
                    cell.version = cell.version.wrapping_add(1);
                    break;
                }
            }
        }
        Event::PermissionExplained {
            permission,
            text,
            level,
        } => {
            // The answer folds into the still-pending prompt itself: the
            // request stays answerable underneath it, and a re-asked `e`
            // replaces the text instead of stacking another paragraph. Both
            // transcripts are tried — a followed subagent's request also sits
            // in the panel's copy.
            let explain = PermExplain::Done {
                text: text.clone(),
                level,
            };
            let landed = state.transcript.set_perm_explain(&permission, &explain)
                | state
                    .panel
                    .agent_transcript
                    .set_perm_explain(&permission, &explain);
            if !landed {
                // The request was answered before the aux call finished, so
                // no prompt is left to carry the result — land it as a notice
                // rather than dropping the answer silently. A `None` level is
                // the daemon's honest-failure text, which already says what
                // it is; a rating gets the `explain · <level>:` prefix the
                // prompt would have shown.
                let text = match level {
                    Some(lv) => format!(
                        "explain · {}: {text}",
                        crate::state::transcript::explain_level_name(lv)
                    ),
                    None => text,
                };
                // It answers the user's `e`, so it scrolls into view like
                // every other notice.
                state.push_notice_with_level(ToastLevel::Info, text);
            }
        }
        Event::PlanUpdated(plan) => {
            state.rail.plan = plan;
        }
        Event::AgentAdded(info) => {
            state.rail.upsert_agent(info);
        }
        Event::AgentUpdated(info) => {
            // Following a subagent stops when that subagent stops. No toast here:
            // `observe_background_completion` already puts the finished agent in
            // the transcript, and saying it twice was noise.
            if !info.status.is_live()
                && let Focus::Panel {
                    target: PanelTarget::Agent(ref id),
                    ref mut follow,
                    ..
                } = state.focus
                && *id == info.id
            {
                *follow = false;
            }
            if !info.id.is_main() && !info.status.is_live() {
                state
                    .rail
                    .mark_agent_terminal(&info.id, pacode_types::time::now_ms());
            }
            state.rail.upsert_agent(info);
        }
        Event::TaskAdded(info) => {
            state.rail.upsert_task(info);
        }
        Event::TaskUpdated(info) => {
            // The transcript line from `observe_background_completion` is the
            // single report; a toast on top of it was the same text twice.
            state.rail.upsert_task(info);
        }
        Event::QuestionAsked(question) => {
            // The turn is blocked on this, so it takes the keyboard
            // immediately. It joins the queue — an ask that was already
            // waiting is never thrown away for the new one.
            if state.questions.push(question) {
                state.focus = Focus::Overlay(crate::state::Overlay::QuestionPicker);
            }
            state.dirty = true;
        }
        Event::QuestionResolved { question, answer } => {
            // Answered here or on another client — either way it leaves the
            // queue. The picker stays up while more questions wait.
            state.questions.resolve(&question);
            if state.questions.is_empty()
                && matches!(
                    state.focus,
                    Focus::Overlay(crate::state::Overlay::QuestionPicker)
                )
            {
                state.focus = Focus::Normal;
            }
            let _ = answer;
            state.dirty = true;
        }
        Event::CronUpdated(job) => {
            state.rail.upsert_cron_job(job);
            state.dirty = true;
        }
        Event::CronRemoved(id) => {
            state.rail.remove_cron_job(&id);
            state.dirty = true;
        }
        Event::MonitorUpdated(info) => {
            state.rail.upsert_monitor(info);
            state.dirty = true;
        }
        Event::UsageUpdated(usage) => {
            state.rail.usage = usage;
        }
        Event::Toast {
            level,
            title,
            detail,
        } => {
            state.push_toast(level, title, detail, now);
        }
        Event::DaemonShuttingDown => {
            state.connection = crate::state::Connection::Disconnected {
                reason: "Daemon shutting down".into(),
            };
        }
        Event::PluginToast { plugin, text } => {
            state.push_toast(pacode_types::ToastLevel::Info, text, Some(plugin), now);
        }
        Event::PluginStatus { plugin, text } => {
            if text.is_empty() {
                state.plugin_status = None;
            } else {
                state.plugin_status = Some((plugin, text));
            }
        }
        Event::LoginProgress { provider, stage } => {
            state.apply_login_progress(provider, stage, now);
        }
        Event::AuthUpdated(info) => {
            if let Some(pos) = state.auth_providers.iter().position(|p| p.id == info.id) {
                state.auth_providers[pos] = info;
            } else {
                state.auth_providers.push(info);
            }
            state.dirty = true;
        }
        Event::ModelsUpdated { models } => {
            // The daemon pushed a fresh catalog (a force-refresh or a
            // background refresh landing) — fold like a `Reply::Models`.
            state.models = models;
            state.lists.on_reply(crate::cache::ListKind::Models, true);
            crate::cache::write_after_reply(state, crate::cache::ListKind::Models);
            state.dirty = true;
        }
        Event::SessionStatus { session, status } => {
            state.apply_session_status(session, status);
        }
        Event::NoticeLockChanged {
            session,
            locked,
            held,
            dropped,
        } => {
            state.notice_lock.apply(session, locked, held, dropped);
        }
        Event::ContextCleared => apply_context_cleared(state),
    }
    let turn_active = state.turn_active;
    state.rail.update_idle(turn_active, now);
}

/// The daemon reset the conversation (`/clear`, from this client or another):
/// drop everything drawn from it. Cron jobs, monitors and tasks are not
/// conversation and stay; so do the session's spend totals.
fn apply_context_cleared(state: &mut AppState) {
    state.transcript.reset(Vec::new(), false);
    state.selection.clear();
    state.rail.plan = pacode_types::Plan::default();
    let main = state
        .rail
        .agents
        .iter()
        .filter(|agent| agent.id.is_main())
        .cloned()
        .collect();
    state.rail.replace_agents(main);
    state.rail.usage.context_tokens = 0;
    // A panel target `Agent` is always a subagent, and every subagent is gone.
    // The agent transcript goes whatever the panel shows: it may still hold a
    // subagent's stream from before the panel moved to a task or a diff.
    if matches!(state.panel.target, Some(PanelTarget::Agent(_))) {
        state.panel.target = None;
    }
    state.panel.drop_agent_transcript();
    if matches!(
        state.focus,
        Focus::SelectAgent { .. }
            | Focus::Panel {
                target: PanelTarget::Agent(_),
                ..
            }
    ) {
        state.focus = Focus::Normal;
    }
    state.dirty = true;
}

/// Where an agent's items, deltas and tool output land: the main agent's go to
/// the session transcript, the agent the panel shows — whatever holds the
/// keyboard meanwhile — to the panel's copy. Any other subagent's are dropped;
/// `LoadPanel` fetches its history when its panel opens.
fn transcript_for_agent<'a>(
    state: &'a mut AppState,
    agent: &AgentId,
) -> Option<&'a mut Transcript> {
    if agent.is_main() {
        return Some(&mut state.session.transcript);
    }
    match state.panel_agent_target() {
        Some(PanelTarget::Agent(id)) if id == *agent => {
            Some(state.session.panel.agent_transcript_for(agent))
        }
        Some(
            PanelTarget::Agent(_)
            | PanelTarget::Task(_)
            | PanelTarget::McpServer(_)
            | PanelTarget::FileDiff { .. }
            | PanelTarget::CommitDiff { .. },
        )
        | None => None,
    }
}

fn compute_turn_stats(duration_ms: u64, usage: &pacode_types::stream::Usage) -> Option<String> {
    if usage.output_tokens == 0 {
        return None;
    }
    Some(stats::stats_line(duration_ms))
}

fn attach_turn_stats(transcript: &mut Transcript, stats_str: String) {
    if let Some(cell) = transcript
        .cells
        .iter_mut()
        .rev()
        .find(|c| matches!(c.kind, CellKind::Item(TranscriptKind::Assistant { .. })))
    {
        cell.stats = Some(stats_str);
        cell.version = cell.version.wrapping_add(1);
    }
}
