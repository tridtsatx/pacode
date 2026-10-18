//! Answering tool calls that never ran: the `cancelled` stub result plus the
//! transcript row update.

use pacode_types::{CallId, Event, Message, ToolStatus, TranscriptItem, TranscriptKind};

use crate::agent::Agent;
use crate::session::Session;

pub(super) struct CancelledCall {
    pub(super) call_id: CallId,
    pub(super) name: String,
    pub(super) title: String,
    pub(super) intent: Option<String>,
    pub(super) item_seq: u64,
}

/// Answer a call that never ran (or was cut short) with a `cancelled` tool result and
/// flip its transcript row to an error.
pub(super) async fn record_cancelled(session: &Session, agent: &Agent, call: CancelledCall) {
    let stub = Message::tool_result(call.call_id.clone(), "cancelled", true);
    let (seq, arc_stub) = agent
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(stub);
    let _ = session
        .store
        .append_message(&session.id, &agent.id(), seq, &arc_stub)
        .await;
    let item = TranscriptItem {
        seq: call.item_seq,
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind: TranscriptKind::ToolCall {
            call_id: call.call_id,
            name: call.name,
            title: call.title,
            intent: call.intent,
            status: ToolStatus::Error,
            preview: "cancelled".to_string(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemUpdated(item));
}
