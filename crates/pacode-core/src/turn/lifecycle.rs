//! Turn spawning and error notification lifecycle helpers.

use std::sync::Arc;

use pacode_types::{AgentStatus, Event, Role, TurnStop};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;

/// Surface a turn failure in the transcript (error notice item) so clients and
/// `pacode run` see why nothing happened.
pub(crate) fn emit_failure_notice(session: &Session, agent: &Agent, message: &str) {
    let seq = agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .next_seq();
    let item = pacode_types::TranscriptItem {
        seq,
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind: pacode_types::TranscriptKind::Notice {
            level: pacode_types::ToastLevel::Error,
            text: format!("turn failed: {message}"),
        },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemAdded(item));
}

/// Spawn `run_turn` as a tokio task, storing the cancel token on the agent. Returns
/// immediately. When the turn ends, the token is cleared and, for subagents, the
/// parent gets an `Injection::AgentFinished` and the agent's history is persisted
/// and dropped from memory (info + summary stay).
pub fn start_turn(session: Arc<Session>, agent: Arc<Agent>) {
    let cancel = CancellationToken::new();
    *agent.cancel.lock().unwrap_or_else(|p| p.into_inner()) = Some(cancel.clone());

    if agent.id().is_main() {
        let mut hbt = agent
            .history_before_turn
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if hbt.is_none() {
            *hbt = Some(
                agent
                    .history
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone(),
            );
        }
    }

    tokio::spawn(async move {
        let stop = super::run_turn(session.clone(), agent.clone(), cancel).await;
        *agent.cancel.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *agent
            .history_before_turn
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;

        // An injection that landed between the last drain and the end of the turn
        // would otherwise wait for the next user message: run another turn for it.
        if agent.id().is_main()
            && matches!(stop, TurnStop::Completed)
            && !agent.injections.is_empty()
        {
            start_turn(session.clone(), agent.clone());
            return;
        }

        if !agent.id().is_main() {
            let last_assistant_text = {
                let hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
                hist.messages
                    .iter()
                    .rev()
                    .find_map(|m| {
                        if m.role == Role::Assistant {
                            Some(m.text())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default()
            };
            let capped_answer = pacode_types::truncate_head_tail(
                &last_assistant_text,
                session.config.context.injection_cap_chars,
            );

            {
                if let Ok(mut info) = agent.info.write() {
                    info.summary = Some(capped_answer.clone());
                    info.status = AgentStatus::Finished;
                    info.finished_at_ms = Some(pacode_types::now_ms());
                }
            }
            let info = agent.info();
            let _ = session
                .store
                .upsert_agent(&session.id, &info, agent.prompt().as_deref())
                .await;
            session.events.emit(Event::AgentUpdated(info.clone()));

            if let Some(parent_id) = &info.parent
                && let Some(parent) = session.agent(parent_id)
            {
                parent
                    .injections
                    .push(crate::inject::Injection::AgentFinished {
                        agent: info,
                        answer: capped_answer,
                    });
                if parent.id().is_main() && !parent.is_running() {
                    start_turn(session.clone(), parent);
                }
            }

            {
                let mut hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
                hist.messages.clear();
            }
        }
    });
}
