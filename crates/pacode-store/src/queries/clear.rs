//! Resetting a session's conversation in place (`Request::ClearContext`).

use pacode_types::{AgentId, SessionId};
use rusqlite::types::Value;
use rusqlite::{Connection, params};

use crate::StoreError;
use crate::txn::Txn;

#[cfg(test)]
#[path = "clear_tests.rs"]
mod clear_tests;

/// Forget a session's conversation, in one transaction: every message of
/// every agent (subagents are dropped with the conversation), every subagent
/// row, every compaction summary and the plan. The caller has every writer of
/// these rows locked out, so "every" needs no boundary — a seq cut-off taken
/// from memory could only lag the store and leave rows behind.
///
/// The rows are deleted rather than fenced off behind a persisted boundary.
/// The UI transcript has no table of its own — a resume rebuilds it from
/// `messages` — and the search index follows `messages` through its triggers,
/// so deleting the rows is the whole job; `delete_session` and detach already
/// purge messages the same way. A boundary would instead have to be honoured
/// by every reader of `messages` (hydration, both history pagers, search,
/// `max_message_seq`, admission recovery), and the first reader to forget it
/// would bring the cleared conversation back.
///
/// Message admissions stay: they are the idempotency record a replayed client
/// message is answered from. They lose their `message_seq`, though — the rows
/// they pointed at are gone, and a resumed session restarts its seq counter
/// from an empty table, so an `interrupted` admission still holding seq 3
/// would hide the new message that takes seq 3. Usage rows stay too: they are
/// what the session cost, not what the model remembers.
pub fn clear_context(conn: &mut Connection, session: &SessionId) -> Result<(), StoreError> {
    let tx = Txn::open(conn)?;
    super::message::purge_messages(
        &tx,
        "session_id = ?1",
        &[Value::Text(session.as_str().to_string())],
    )?;
    tx.execute(
        "UPDATE message_admissions SET message_seq = NULL
         WHERE session_id = ?1 AND message_seq IS NOT NULL;",
        params![session.as_str()],
    )?;
    tx.execute(
        "DELETE FROM compaction WHERE session_id = ?1;",
        params![session.as_str()],
    )?;
    tx.execute(
        "DELETE FROM agents WHERE session_id = ?1 AND id <> ?2;",
        params![session.as_str(), AgentId::main().as_str()],
    )?;
    tx.execute(
        "DELETE FROM plans WHERE session_id = ?1;",
        params![session.as_str()],
    )?;
    tx.commit()?;
    Ok(())
}
