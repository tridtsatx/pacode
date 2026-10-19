//! Message and FTS queries for `Store`.

use std::collections::HashSet;

use pacode_types::{AgentId, ContentBlock, Message, Role, SessionId, TurnId};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use super::{
    AdmissionCursor, MessageAdmission, MessageAdmissionPage, MessageAdmissionResult,
    MessageAdmissionState, MessagePage, MessageRow, SearchHit,
};
use crate::StoreError;
use crate::txn::Txn;

pub const DEFAULT_MESSAGE_PAGE: u32 = 200;
pub const DEFAULT_ADMISSION_PAGE: u32 = 1000;

const MESSAGE_PURGE_CHUNK: usize = 256;
const ADMISSION_COLUMNS: &str =
    "session_id, message_id, fingerprint, agent_id, message_seq, state, turn_id, terminal_stop";

pub(crate) fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// Max bytes of one tool result that are kept in `messages.text` and so indexed in
/// FTS (head only, cut on a char boundary). Text blocks are indexed in full. The FTS
/// triggers replay `messages.text` on delete, so changing the cap needs no rebuild:
/// old rows keep the text they were indexed with.
pub(crate) const FTS_TOOL_RESULT_CAP: usize = 2048;

fn head(text: &str, cap: usize) -> &str {
    if text.len() <= cap {
        return text;
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The text indexed in FTS for `msg` (see [`FTS_TOOL_RESULT_CAP`]).
pub(crate) fn message_plain_text(msg: &Message) -> String {
    let mut parts: Vec<std::borrow::Cow<'_, str>> = Vec::new();
    for block in &msg.content {
        match block {
            ContentBlock::Text { text } => parts.push(text.as_str().into()),
            ContentBlock::ToolResult { content, .. } => {
                parts.push(head(content.as_str(), FTS_TOOL_RESULT_CAP).into())
            }
            // Index the placeholder so an attached file name stays searchable.
            ContentBlock::Image { source } => parts.push(source.placeholder().into()),
            ContentBlock::Reasoning { .. } | ContentBlock::ToolUse { .. } => {}
        }
    }
    parts.join("\n")
}

/// Drops every `messages` row matching `predicate`. The search index follows the
/// table through the `messages` triggers, so a purge never has to hand back the
/// text that was indexed. Rows are taken in bounded chunks by `id`, so deleting a
/// session or a detach tail never holds more than [`MESSAGE_PURGE_CHUNK`] ids in
/// memory and never re-reads a row it already deleted.
pub(crate) fn purge_messages(
    conn: &Connection,
    predicate: &str,
    binds: &[Value],
) -> Result<u64, StoreError> {
    let mut cursor = Value::Integer(0);
    let mut removed = 0u64;
    loop {
        let cursor_index = binds.len() + 1;
        let mut stmt = conn.prepare(&format!(
            "DELETE FROM messages
             WHERE id IN (
                 SELECT id FROM messages
                 WHERE ({predicate}) AND id > ?{cursor_index}
                 ORDER BY id ASC
                 LIMIT {MESSAGE_PURGE_CHUNK}
             )
             RETURNING id;"
        ))?;
        let mut last_id: Option<i64> = None;
        {
            let mut rows = stmt.query(params_from_iter(
                binds.iter().chain(std::iter::once(&cursor)),
            ))?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                last_id = Some(last_id.map_or(id, |seen: i64| seen.max(id)));
                removed += 1;
            }
        }
        let Some(id) = last_id else {
            return Ok(removed);
        };
        cursor = Value::Integer(id);
    }
}

/// Appends inside a unit of work the caller already owns: a transaction of its
/// own, or a savepoint inside the writer's batch transaction. Either way this
/// function must not open one, or the group commit would nest `BEGIN` in
/// `BEGIN`; the atomicity it needs is the one it was handed.
pub(crate) fn append_message_in_unit_of_work(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
    seq: u64,
    msg: &Message,
) -> Result<(), StoreError> {
    let content_json = serde_json::to_string(msg)?;
    let existing: Option<String> = conn
        .prepare_cached(
            "SELECT content_json FROM messages WHERE session_id = ?1 AND agent_id = ?2 AND seq = ?3;",
        )?
        .query_row(
            params![session.as_str(), agent.as_str(), seq as i64],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(old_json) = existing
        && old_json != content_json
    {
        return Err(StoreError::Invalid(format!(
            "message seq collision for {}/{}/{}",
            session.as_str(),
            agent.as_str(),
            seq
        )));
    }

    let hidden = if msg.meta.hidden { 1 } else { 0 };
    let role = role_to_str(msg.role);

    conn.prepare_cached(
        r#"
        INSERT INTO messages (session_id, agent_id, seq, role, content_json, text, hidden, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(session_id, agent_id, seq) DO UPDATE SET
            role = excluded.role,
            content_json = excluded.content_json,
            text = excluded.text,
            hidden = excluded.hidden,
            created_at = excluded.created_at;
        "#,
    )?
    .execute(params![
        session.as_str(),
        agent.as_str(),
        seq as i64,
        role,
        content_json,
        message_plain_text(msg),
        hidden,
        msg.meta.timestamp_ms as i64,
    ])?;
    Ok(())
}

pub fn append_message(
    conn: &mut Connection,
    session: &SessionId,
    agent: &AgentId,
    seq: u64,
    msg: &Message,
) -> Result<(), StoreError> {
    let tx = Txn::open(conn)?;
    append_message_in_unit_of_work(&tx, session, agent, seq, msg)?;
    tx.commit()?;
    Ok(())
}

pub fn append_user_message(
    conn: &mut Connection,
    session: &SessionId,
    agent: &AgentId,
    seq: u64,
    message_id: &str,
    fingerprint: u64,
    msg: &Message,
) -> Result<MessageAdmissionResult, StoreError> {
    append_user_message_with_turn(
        conn,
        session,
        agent,
        seq,
        message_id,
        fingerprint,
        None,
        msg,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn append_user_message_with_turn(
    conn: &mut Connection,
    session: &SessionId,
    agent: &AgentId,
    seq: u64,
    message_id: &str,
    fingerprint: u64,
    turn_id: Option<&str>,
    msg: &Message,
) -> Result<MessageAdmissionResult, StoreError> {
    let tx = Txn::open(conn)?;
    let inserted = tx.execute(
        "INSERT INTO message_admissions (session_id, message_id, fingerprint, agent_id, message_seq, state, turn_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'admitted', ?6, ?7) ON CONFLICT(session_id, message_id) DO NOTHING;",
        params![
            session.as_str(),
            message_id,
            fingerprint.to_string(),
            agent.as_str(),
            seq as i64,
            turn_id,
            pacode_types::now_ms() as i64,
        ],
    )?;
    if inserted == 0 {
        let existing: String = tx.query_row(
            "SELECT fingerprint FROM message_admissions WHERE session_id = ?1 AND message_id = ?2;",
            params![session.as_str(), message_id],
            |row| row.get(0),
        )?;
        tx.commit()?;
        return Ok(if existing == fingerprint.to_string() {
            MessageAdmissionResult::Duplicate
        } else {
            MessageAdmissionResult::Conflict
        });
    }

    append_message_in_unit_of_work(&tx, session, agent, seq, msg)?;
    tx.commit()?;
    Ok(MessageAdmissionResult::Appended)
}

fn map_message_admission(row: &rusqlite::Row<'_>) -> Result<MessageAdmission, rusqlite::Error> {
    let session: String = row.get(0)?;
    let message_id: String = row.get(1)?;
    let fingerprint: String = row.get(2)?;
    let agent: String = row.get(3)?;
    let message_seq: Option<i64> = row.get(4)?;
    let state: String = row.get(5)?;
    let turn_id: Option<String> = row.get(6)?;
    let terminal_stop: Option<String> = row.get(7)?;
    let fingerprint = fingerprint.parse::<u64>().map_err(|_| {
        rusqlite::Error::InvalidParameterName(format!(
            "invalid fingerprint for message admission {session}/{message_id}"
        ))
    })?;
    let state = MessageAdmissionState::from_str(&state).map_err(|error| {
        rusqlite::Error::InvalidParameterName(format!("{error}: {session}/{message_id}"))
    })?;
    Ok(MessageAdmission {
        session: SessionId::new(session),
        message_id,
        fingerprint,
        agent: AgentId::new(agent),
        message_seq: message_seq.map(|seq| seq as u64),
        state,
        turn_id: turn_id.map(TurnId::new),
        terminal_stop,
    })
}

pub fn get_message_admission(
    conn: &Connection,
    session: &SessionId,
    message_id: &str,
) -> Result<Option<MessageAdmission>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT session_id, message_id, fingerprint, agent_id, message_seq, state, turn_id, terminal_stop FROM message_admissions WHERE session_id = ?1 AND message_id = ?2;",
    )?;
    let row = stmt
        .query_row(params![session.as_str(), message_id], map_message_admission)
        .optional()?;
    Ok(row)
}

fn collect_admissions<M>(rows: M) -> Result<Vec<MessageAdmission>, StoreError>
where
    M: Iterator<Item = rusqlite::Result<MessageAdmission>>,
{
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn load_message_admissions(conn: &Connection) -> Result<Vec<MessageAdmission>, StoreError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ADMISSION_COLUMNS} FROM message_admissions ORDER BY session_id, message_seq, message_id;"
    ))?;
    collect_admissions(stmt.query_map([], map_message_admission)?)
}

pub fn load_message_admissions_for_session(
    conn: &Connection,
    session: &SessionId,
) -> Result<Vec<MessageAdmission>, StoreError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ADMISSION_COLUMNS} FROM message_admissions WHERE session_id = ?1 ORDER BY message_seq, message_id;"
    ))?;
    collect_admissions(stmt.query_map(params![session.as_str()], map_message_admission)?)
}

/// Admissions of one agent that never reached a terminal state, in the order the
/// agent has to replay them.
pub fn load_unresolved_message_admissions(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
) -> Result<Vec<MessageAdmission>, StoreError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ADMISSION_COLUMNS} FROM message_admissions WHERE session_id = ?1 AND agent_id = ?2 AND state <> 'terminal' ORDER BY message_seq, message_id;"
    ))?;
    collect_admissions(stmt.query_map(
        params![session.as_str(), agent.as_str()],
        map_message_admission,
    )?)
}

/// Bounded keyset scan over every admission, oldest key first.
pub fn load_message_admissions_page(
    conn: &Connection,
    limit: u32,
    cursor: Option<&AdmissionCursor>,
) -> Result<MessageAdmissionPage, StoreError> {
    let page = if limit == 0 {
        DEFAULT_ADMISSION_PAGE
    } else {
        limit
    };
    let cap = page as i64;
    let cursor_session: Option<&str> = cursor.map(|cursor| cursor.session.as_str());
    let cursor_seq: i64 = cursor
        .and_then(|cursor| cursor.message_seq)
        .map_or(-1, |seq| seq as i64);
    let cursor_message: &str = cursor.map_or("", |cursor| cursor.message_id.as_str());

    let mut stmt = conn.prepare_cached(&format!(
        r#"
        SELECT {ADMISSION_COLUMNS}
        FROM message_admissions
        WHERE ?1 IS NULL
           OR session_id > ?1
           OR (session_id = ?1 AND (
                  COALESCE(message_seq, -1) > ?2
                  OR (COALESCE(message_seq, -1) = ?2 AND message_id > ?3)))
        ORDER BY session_id ASC, message_seq ASC, message_id ASC
        LIMIT ?4;
        "#
    ))?;
    let mut admissions = collect_admissions(stmt.query_map(
        params![cursor_session, cursor_seq, cursor_message, cap + 1],
        map_message_admission,
    )?)?;

    let has_more = admissions.len() as i64 > cap;
    if has_more {
        admissions.truncate(cap as usize);
    }
    let next_cursor = if has_more {
        admissions.last().map(|admission| AdmissionCursor {
            session: admission.session.clone(),
            message_seq: admission.message_seq,
            message_id: admission.message_id.clone(),
        })
    } else {
        None
    };
    Ok(MessageAdmissionPage {
        admissions,
        next_cursor,
    })
}

pub fn mark_message_admission_running(
    conn: &Connection,
    session: &SessionId,
    message_id: &str,
    turn_id: &str,
) -> Result<bool, StoreError> {
    let changed = conn.execute(
        "UPDATE message_admissions SET state = 'running', turn_id = ?3 WHERE session_id = ?1 AND message_id = ?2 AND state = 'admitted';",
        params![session.as_str(), message_id, turn_id],
    )?;
    Ok(changed == 1)
}

pub fn mark_message_admission_terminal(
    conn: &Connection,
    session: &SessionId,
    message_id: &str,
    terminal_stop: Option<&str>,
) -> Result<bool, StoreError> {
    let changed = conn.execute(
        "UPDATE message_admissions SET state = 'terminal', terminal_stop = COALESCE(?3, terminal_stop) WHERE session_id = ?1 AND message_id = ?2 AND state != 'terminal';",
        params![session.as_str(), message_id, terminal_stop],
    )?;
    Ok(changed == 1)
}

pub fn cancelled_message_seqs(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
) -> Result<HashSet<u64>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT message_seq FROM message_admissions WHERE session_id = ?1 AND agent_id = ?2 AND state = 'terminal' AND terminal_stop IN ('interrupted', 'cancelled', 'skipped') AND message_seq IS NOT NULL;",
    )?;
    let rows = stmt.query_map(params![session.as_str(), agent.as_str()], |row| {
        Ok(row.get::<_, i64>(0)? as u64)
    })?;
    Ok(rows.collect::<Result<HashSet<_>, _>>()?)
}

pub fn load_messages(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
) -> Result<Vec<MessageRow>, StoreError> {
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT seq, content_json FROM messages
        WHERE session_id = ?1 AND agent_id = ?2
        ORDER BY seq ASC;
        "#,
    )?;
    let rows = stmt.query_map(params![session.as_str(), agent.as_str()], |row| {
        let seq: i64 = row.get(0)?;
        let json: String = row.get(1)?;
        Ok((seq, json))
    })?;
    collect_message_rows(rows)
}

fn collect_message_rows<M>(rows: M) -> Result<Vec<MessageRow>, StoreError>
where
    M: Iterator<Item = rusqlite::Result<(i64, String)>>,
{
    let mut result = Vec::new();
    for row in rows {
        let (seq, json) = row?;
        let message: Message = serde_json::from_str(&json)?;
        result.push(MessageRow {
            seq: seq as u64,
            message,
        });
    }
    Ok(result)
}

fn collect_message_page(
    conn: &Connection,
    sql: &str,
    binds: &[Value],
) -> Result<Vec<MessageRow>, StoreError> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params_from_iter(binds.iter()), |row| {
        let seq: i64 = row.get(0)?;
        let json: String = row.get(1)?;
        Ok((seq, json))
    })?;
    collect_message_rows(rows)
}

/// Up to `limit` messages with `seq < before` (or the last ones), ascending.
pub fn load_messages_before(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
    before: Option<u64>,
    limit: u32,
) -> Result<Vec<MessageRow>, StoreError> {
    let limit = if limit == 0 { 200 } else { limit };
    let session_id = Value::Text(session.as_str().to_string());
    let agent_id = Value::Text(agent.as_str().to_string());
    let mut rows = match before {
        Some(before_seq) => collect_message_page(
            conn,
            r#"
            SELECT seq, content_json FROM messages
            WHERE session_id = ?1 AND agent_id = ?2 AND seq < ?3
            ORDER BY seq DESC
            LIMIT ?4;
            "#,
            &[
                session_id.clone(),
                agent_id.clone(),
                Value::Integer(before_seq as i64),
                Value::Integer(limit as i64),
            ],
        )?,
        None => collect_message_page(
            conn,
            r#"
            SELECT seq, content_json FROM messages
            WHERE session_id = ?1 AND agent_id = ?2
            ORDER BY seq DESC
            LIMIT ?3;
            "#,
            &[
                session_id.clone(),
                agent_id.clone(),
                Value::Integer(limit as i64),
            ],
        )?,
    };
    rows.reverse();
    Ok(rows)
}

/// Bounded forward read of live rows: the rows with `seq > after`, ascending,
/// skipping the messages of admissions that ended interrupted, cancelled or
/// skipped. Resuming after a compaction passes the summary's `upto_seq` here
/// instead of reading the whole transcript and filtering it away. A page that runs
/// into discarded rows is refilled past them, so the caller gets `limit` usable
/// rows or an exhausted window, never a short page caused by discards alone.
pub fn load_messages_after(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
    after: Option<u64>,
    limit: u32,
) -> Result<MessagePage, StoreError> {
    let page = if limit == 0 {
        DEFAULT_MESSAGE_PAGE
    } else {
        limit
    };
    let cap = page as i64;
    let session_id = Value::Text(session.as_str().to_string());
    let agent_id = Value::Text(agent.as_str().to_string());

    let mut rows: Vec<MessageRow> = Vec::new();
    let mut cursor = after;
    let mut has_more = false;
    while (rows.len() as i64) < cap {
        let want = cap - rows.len() as i64;
        let mut batch = match cursor {
            Some(after_seq) => collect_message_page(
                conn,
                r#"
                SELECT seq, content_json FROM messages
                WHERE session_id = ?1 AND agent_id = ?2 AND seq > ?3
                  AND NOT EXISTS (SELECT 1 FROM message_admissions AS admission
                                  WHERE admission.session_id = ?1
                                    AND admission.agent_id = ?2
                                    AND admission.message_seq = messages.seq
                                    AND admission.state = 'terminal'
                                    AND admission.terminal_stop IN ('interrupted', 'cancelled', 'skipped'))
                ORDER BY seq ASC
                LIMIT ?4;
                "#,
                &[
                    session_id.clone(),
                    agent_id.clone(),
                    Value::Integer(after_seq as i64),
                    Value::Integer(want + 1),
                ],
            )?,
            None => collect_message_page(
                conn,
                r#"
                SELECT seq, content_json FROM messages
                WHERE session_id = ?1 AND agent_id = ?2
                  AND NOT EXISTS (SELECT 1 FROM message_admissions AS admission
                                  WHERE admission.session_id = ?1
                                    AND admission.agent_id = ?2
                                    AND admission.message_seq = messages.seq
                                    AND admission.state = 'terminal'
                                    AND admission.terminal_stop IN ('interrupted', 'cancelled', 'skipped'))
                ORDER BY seq ASC
                LIMIT ?3;
                "#,
                &[
                    session_id.clone(),
                    agent_id.clone(),
                    Value::Integer(want + 1),
                ],
            )?,
        };
        let Some(last) = batch.last().map(|row| row.seq) else {
            break;
        };
        cursor = Some(last);
        if batch.len() as i64 > want {
            batch.truncate(want as usize);
            has_more = true;
        }
        rows.extend(batch);
        if has_more {
            break;
        }
    }
    Ok(MessagePage { rows, has_more })
}

/// Highest stored `seq` for the agent, independent of any window.
pub fn max_message_seq(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
) -> Result<Option<u64>, StoreError> {
    let max_seq: Option<i64> = conn.query_row(
        "SELECT MAX(seq) FROM messages WHERE session_id = ?1 AND agent_id = ?2;",
        params![session.as_str(), agent.as_str()],
        |row| row.get(0),
    )?;
    Ok(max_seq.map(|seq| seq as u64))
}

fn find_case_insensitive(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let needle_chars: Vec<char> = needle.chars().collect();
    let haystack_chars: Vec<(usize, char)> = haystack.char_indices().collect();

    for i in 0..haystack_chars.len() {
        if haystack_chars.len() - i < needle_chars.len() {
            break;
        }
        let matches = needle_chars.iter().enumerate().all(|(j, &nc)| {
            haystack_chars[i + j]
                .1
                .to_lowercase()
                .zip(nc.to_lowercase())
                .all(|(hc, n)| hc == n)
        });
        if matches {
            let start_byte = haystack_chars[i].0;
            let end_byte = if i + needle_chars.len() < haystack_chars.len() {
                haystack_chars[i + needle_chars.len()].0
            } else {
                haystack.len()
            };
            return Some((start_byte, end_byte));
        }
    }
    None
}

fn fallback_snippet(text: &str, query: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let term = query.split_whitespace().next().unwrap_or(query);
    if let Some((start, end)) = find_case_insensitive(text, term) {
        let matched = &text[start..end];
        let prefix = &text[..start];
        let suffix = &text[end..];

        let prefix_words: Vec<&str> = prefix.split_whitespace().collect();
        let head = if prefix_words.len() > 6 {
            let start_idx = prefix_words.len().saturating_sub(6);
            format!("… {}", prefix_words[start_idx..].join(" "))
        } else if prefix_words.is_empty() {
            String::new()
        } else {
            prefix_words.join(" ")
        };

        let suffix_words: Vec<&str> = suffix.split_whitespace().collect();
        let tail = if suffix_words.len() > 6 {
            format!("{} …", suffix_words[..6].join(" "))
        } else if suffix_words.is_empty() {
            String::new()
        } else {
            suffix_words.join(" ")
        };

        let mut res = String::new();
        if !head.is_empty() {
            res.push_str(&head);
            res.push(' ');
        }
        res.push('[');
        res.push_str(matched);
        res.push(']');
        if !tail.is_empty() {
            res.push(' ');
            res.push_str(&tail);
        }
        res
    } else {
        let words: Vec<&str> = text.split_whitespace().take(12).collect();
        words.join(" ")
    }
}

pub fn search(conn: &Connection, query: &str, limit: u32) -> Result<Vec<SearchHit>, StoreError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let limit = if limit == 0 { 200 } else { limit };
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT m.session_id, m.agent_id, m.seq, snippet(messages_fts, 0, '[', ']', '…', 12), m.text
        FROM messages_fts
        JOIN messages m ON m.id = messages_fts.rowid
        WHERE messages_fts MATCH ?1
        ORDER BY rank
        LIMIT ?2;
        "#,
    )?;

    let rows = stmt.query_map(params![trimmed, limit], |row| {
        let session: String = row.get(0)?;
        let agent: String = row.get(1)?;
        let seq: i64 = row.get(2)?;
        let raw_snippet: Option<String> = row.get(3)?;
        let text: String = row.get(4)?;
        Ok((session, agent, seq, raw_snippet, text))
    })?;

    let mut hits = Vec::new();
    for row in rows {
        let (session, agent, seq, raw_snippet, text) = row?;
        let snippet = match raw_snippet {
            Some(s) if !s.is_empty() => s,
            _ => fallback_snippet(&text, trimmed),
        };
        hits.push(SearchHit {
            session: SessionId::new(session),
            agent: AgentId::new(agent),
            seq: seq as u64,
            snippet,
        });
    }

    Ok(hits)
}
