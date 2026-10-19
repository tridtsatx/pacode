//! Injections that woke an idle agent.
//!
//! An idle agent is woken by `start_turn` after something was pushed on its
//! injection queue (a finished subagent, a question, a task). Its history then
//! ends with its own last reply, so the queue has to be folded into the history
//! before the first request; otherwise the model is called without ever seeing
//! what woke it.

use std::sync::Arc;

use pacode_types::Role;

use crate::agent::Agent;
use crate::session::Session;

/// Fold queued injections into the history when it ends with an assistant message.
pub(crate) async fn absorb_pending(session: &Arc<Session>, agent: &Arc<Agent>) {
    if agent.injections.is_empty() {
        return;
    }
    let ends_with_assistant = agent
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .messages
        .last()
        .is_some_and(|m| m.role == Role::Assistant);
    if !ends_with_assistant {
        return;
    }
    let drained = agent.injections.drain();
    if drained.is_empty() {
        return;
    }
    let message = crate::inject::render_injections(
        &drained,
        session.config.context.injection_cap_chars,
        session.config.context.injection_budget_chars,
    );
    let (seq, arc) = agent
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(message);
    let _ = session
        .store
        .append_message(&session.id, &agent.id(), seq, &arc)
        .await;
}
