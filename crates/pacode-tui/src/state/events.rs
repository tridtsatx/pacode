//! Folding daemon events into transcript, rail, panel, and toasts.

use std::time::Instant;

use pacode_types::time::now_ms;
use pacode_types::{Event, PermissionDecision, TaskStatus, ToastLevel, TranscriptKind};

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
            let _ = pacode_config::save_prefs(&state.paths, &prefs);
            let header = crate::state::transcript::HeaderInfo {
                version: state.app_version.clone(),
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
            if !info.status.is_live()
                && let Focus::Panel {
                    target: PanelTarget::Agent(ref id),
                    ref mut follow,
                    ..
                } = state.focus
            {
                if *id == info.id {
                    *follow = false;
                } else if *follow {
                    let title = format!("{} finished", info.name);
                    state.push_toast(ToastLevel::Info, title, info.summary.clone(), now);
                }
            }
            state.rail.upsert_agent(info);
        }
        Event::TaskAdded(info) => {
            state.rail.upsert_task(info);
        }
        Event::TaskUpdated(info) => {
            if info.status.is_terminal()
                && let Focus::Panel { follow: true, .. } = state.focus
            {
                let level = match info.status {
                    TaskStatus::Failed => ToastLevel::Error,
                    _ => ToastLevel::Success,
                };
                let duration = pacode_types::time::format_duration_ms(info.duration_ms(now_ms()));
                let title = format!(
                    "{} {}",
                    info.label,
                    if info.status == TaskStatus::Failed {
                        "failed"
                    } else {
                        "completed"
                    }
                );
                state.push_toast(level, title, Some(duration), now);
            }
            state.rail.upsert_task(info);
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
