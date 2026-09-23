//! Folding daemon events into transcript, rail, panel, and toasts.

use std::time::Instant;

use pacode_types::{Event, PermissionDecision, ToastLevel, TranscriptKind};

use crate::state::stats;
use crate::state::transcript::{Cell, CellKind, PermExplain, Transcript};
use crate::state::{AppState, Focus, PanelTarget};

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;

pub fn apply_event(state: &mut AppState, seq: u64, event: Event, now: Instant) {
    match event {
        Event::SessionUpdated(meta) => {
            let mut prefs = pacode_config::load_prefs(&state.paths);
            prefs.model = Some(meta.model.to_string());
            prefs.effort = Some(meta.effort);
            prefs.mode = Some(meta.mode);
            if let Err(e) = pacode_config::save_prefs(&state.paths, &prefs) {
                log::warn!("failed to save prefs on session update: {e}");
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
        Event::TurnStarted { agent, turn: _ } => {
            if agent.is_main() {
                state.turn_active = true;
                state.turn_started_at = Some(now);
                state.rail.update_idle(true, now);
            }
        }
        Event::TurnEnded {
            agent,
            turn: _,
            usage,
            stop,
        } => {
            if let pacode_types::TurnStop::Failed { message } = &stop {
                state.push_toast(
                    ToastLevel::Error,
                    "turn failed".to_string(),
                    Some(message.clone()),
                    now,
                );
            }
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
                state.turn_active = false;
                state.turn_started_at = None;
                state.rail.update_idle(false, now);
            } else {
                let is_panel_target = state.panel.target.as_ref().is_some_and(|t| match t {
                    PanelTarget::Agent(id) => *id == agent,
                    _ => false,
                });
                if is_panel_target {
                    state.panel.agent_transcript.flush_stream();
                    if let Some(ref u) = usage
                        && u.output_tokens > 0
                    {
                        let last_ts = state
                            .panel
                            .agent_transcript
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
                            attach_turn_stats(&mut state.panel.agent_transcript, stats);
                        }
                    }
                }
                // Fail-safe for a dropped `AgentUpdated`: the daemon stamps a
                // subagent's terminal status BEFORE emitting this event
                // (`turn/lifecycle.rs`), so a TurnEnded that finds the rail
                // entry still live means that update was lost — set it
                // locally with the daemon's own mapping (`Completed`→Finished,
                // `Interrupted`→Stopped, `Failed`→Failed) and the same
                // stamp-once rule for `finished_at_ms`.
                if let Some(info) = state.rail.agents.iter_mut().find(|a| a.id == agent)
                    && info.status.is_live()
                {
                    info.status = match &stop {
                        pacode_types::TurnStop::Completed => pacode_types::AgentStatus::Finished,
                        pacode_types::TurnStop::Interrupted => pacode_types::AgentStatus::Stopped,
                        pacode_types::TurnStop::Failed { .. } => pacode_types::AgentStatus::Failed,
                    };
                    if info.finished_at_ms.is_none() {
                        info.finished_at_ms = Some(pacode_types::time::now_ms());
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
            if item.agent.is_main() {
                state.transcript.upsert(item, now);
            } else if let Focus::Panel {
                target: PanelTarget::Agent(ref id),
                ..
            } = state.focus
                && *id == item.agent
            {
                state.panel.agent_transcript.upsert(item, now);
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
            if item.agent.is_main() {
                state.transcript.upsert(item, now);
            } else if let Focus::Panel {
                target: PanelTarget::Agent(ref id),
                ..
            } = state.focus
                && *id == item.agent
            {
                state.panel.agent_transcript.upsert(item, now);
            }
        }
        Event::TextDelta {
            agent,
            item_seq,
            text,
        } => {
            if agent.is_main() {
                state.transcript.push_delta(item_seq, &text, false, now);
            } else if let Focus::Panel {
                target: PanelTarget::Agent(ref id),
                ..
            } = state.focus
                && *id == agent
            {
                state
                    .panel
                    .agent_transcript
                    .push_delta(item_seq, &text, false, now);
            }
        }
        Event::ReasoningDelta {
            agent,
            item_seq,
            text,
        } => {
            if agent.is_main() {
                state.transcript.push_delta(item_seq, &text, true, now);
            } else if let Focus::Panel {
                target: PanelTarget::Agent(ref id),
                ..
            } = state.focus
                && *id == agent
            {
                state
                    .panel
                    .agent_transcript
                    .push_delta(item_seq, &text, true, now);
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
                let now_ms = pacode_types::time::now_ms();
                state.transcript.push_cell(Cell {
                    id: seq,
                    kind: CellKind::Item(TranscriptKind::Notice {
                        level: ToastLevel::Info,
                        text,
                    }),
                    version: 0,
                    ts_ms: now_ms,
                    stats: None,
                    perm_explain: None,
                });
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
            match stage {
                pacode_types::LoginStage::OpenUrl { url, opened } => {
                    let text = if opened {
                        format!("Opened browser to sign in to {provider}: {url}")
                    } else {
                        format!("Open this URL in your browser to sign in to {provider}:\n{url}")
                    };
                    state.push_notice_with_level(ToastLevel::Info, text);
                }
                pacode_types::LoginStage::Waiting => {
                    state.push_notice_with_level(
                        ToastLevel::Info,
                        format!("Waiting for authorization from {provider}..."),
                    );
                }
                pacode_types::LoginStage::Exchanging => {
                    state.push_notice_with_level(
                        ToastLevel::Info,
                        format!("Exchanging authorization code with {provider}..."),
                    );
                }
                pacode_types::LoginStage::Done { label } => {
                    state.push_notice_with_level(
                        ToastLevel::Success,
                        format!("Successfully signed in to {provider} ({label})"),
                    );
                    state.push_toast(
                        ToastLevel::Success,
                        format!("Signed in to {provider}"),
                        Some(label),
                        now,
                    );
                    if matches!(
                        state.focus,
                        Focus::Overlay(crate::state::Overlay::LoginPicker { .. })
                    ) {
                        state.focus = Focus::Normal;
                    }
                }
                pacode_types::LoginStage::Failed { message } => {
                    state.push_notice_with_level(
                        ToastLevel::Error,
                        format!("Login to {provider} failed: {message}"),
                    );
                    state.push_toast(
                        ToastLevel::Error,
                        format!("Login failed: {provider}"),
                        Some(message),
                        now,
                    );
                    if matches!(
                        state.focus,
                        Focus::Overlay(crate::state::Overlay::LoginPicker { .. })
                    ) {
                        state.focus = Focus::Normal;
                    }
                }
            }
            state.dirty = true;
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
    }
    state.rail.update_idle(state.turn_active, now);
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
