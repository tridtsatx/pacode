//! Folding daemon events into transcript, rail, panel, and toasts.

use std::time::Instant;

use pacode_types::{Event, PermissionDecision, ToastLevel, TranscriptKind};

use crate::state::stats;
use crate::state::transcript::{Cell, CellKind, Transcript};
use crate::state::{AppState, Focus, PanelTarget};

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
            }
        }
        Event::ItemAdded(item) => {
            if let TranscriptKind::ToolCall { name, title, .. } = &item.kind {
                let arg = title
                    .strip_prefix(name.as_str())
                    .map(str::trim_start)
                    .unwrap_or_else(|| {
                        title
                            .split_once(' ')
                            .map(|(_, r)| r.trim())
                            .unwrap_or(title.as_str())
                    });
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
            state.transcript.cells.push_back(Cell {
                id: seq,
                kind: CellKind::Item(TranscriptKind::Permission(req)),
                version: 0,
                ts_ms,
                stats: None,
            });
        }
        Event::PermissionResolved {
            permission,
            decision,
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
                        PermissionDecision::Deny => {
                            (ToastLevel::Warn, format!("Denied: {}", req.title))
                        }
                    };
                    cell.kind = CellKind::Item(TranscriptKind::Notice { level, text });
                    cell.version = cell.version.wrapping_add(1);
                    break;
                }
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
            // The turn is blocked on this, so it takes the keyboard immediately.
            state.focus = Focus::Overlay(crate::state::Overlay::QuestionPicker {
                index: question.recommended_index().unwrap_or(0),
                question: Box::new(question),
                selected: Vec::new(),
                typed: String::new(),
                typing: false,
            });
            state.dirty = true;
        }
        Event::QuestionResolved { question, answer } => {
            // Another client may have answered it; drop our picker either way.
            if let Focus::Overlay(crate::state::Overlay::QuestionPicker { question: q, .. }) =
                &state.focus
                && q.id == question
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
