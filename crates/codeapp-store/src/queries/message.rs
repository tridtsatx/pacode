//! Message and FTS queries for `Store`.

use codeapp_types::{AgentId, ContentBlock, Message, Role, SessionId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{MessageRow, SearchHit};
use crate::StoreError;

pub(crate) fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

pub(crate) fn message_plain_text(msg: &Message) -> String {
    let mut parts = Vec::new();
    for block in &msg.content {
        match block {
            ContentBlock::Text { text } => parts.push(text.as_str()),
            ContentBlock::ToolResult { content, .. } => parts.push(content.as_str()),
            ContentBlock::Reasoning { .. } | ContentBlock::ToolUse { .. } => {}
        }
    }
    parts.join("\n")
}

pub(crate) fn extract_message_plain_text(content_json: &str) -> String {
    match serde_json::from_str::<Message>(content_json) {
        Ok(msg) => message_plain_text(&msg),
        Err(_) => String::new(),
    }
}

pub fn append_message(
    conn: &mut Connection,
    session: &SessionId,
    agent: &AgentId,
    seq: u64,
    msg: &Message,
) -> Result<(), StoreError> {
    let tx = conn.transaction()?;
    {
        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, content_json FROM messages WHERE session_id = ?1 AND agent_id = ?2 AND seq = ?3;",
                params![session.as_str(), agent.as_str(), seq as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        if let Some((old_id, old_json)) = existing {
            let old_text = extract_message_plain_text(&old_json);
            tx.execute(
                "INSERT INTO messages_fts(messages_fts, rowid, text) VALUES('delete', ?1, ?2);",
                params![old_id, old_text],
            )?;
        }

        let content_json = serde_json::to_string(msg)?;
        let hidden = if msg.meta.hidden { 1 } else { 0 };
        let role = role_to_str(msg.role);

        let rowid: i64 = tx.query_row(
            r#"
            INSERT INTO messages (session_id, agent_id, seq, role, content_json, hidden, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(session_id, agent_id, seq) DO UPDATE SET
                role = excluded.role,
                content_json = excluded.content_json,
                hidden = excluded.hidden,
                created_at = excluded.created_at
            RETURNING id;
            "#,
            params![
                session.as_str(),
                agent.as_str(),
                seq as i64,
                role,
                content_json,
                hidden,
                msg.meta.timestamp_ms as i64,
            ],
            |r| r.get(0),
        )?;

        let plain_text = message_plain_text(msg);
        tx.execute(
            "INSERT INTO messages_fts(rowid, text) VALUES(?1, ?2);",
            params![rowid, plain_text],
        )?;
    }
    tx.commit()?;
    Ok(())
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

    let mut result = Vec::new();
    for r in rows {
        let (seq, json) = r?;
        let message: Message = serde_json::from_str(&json)?;
        result.push(MessageRow {
            seq: seq as u64,
            message,
        });
    }
    Ok(result)
}

pub fn load_messages_before(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
    before: Option<u64>,
    limit: u32,
) -> Result<Vec<MessageRow>, StoreError> {
    let limit = if limit == 0 { 200 } else { limit };
    let mut rows = match before {
        Some(before_seq) => {
            let mut stmt = conn.prepare_cached(
                r#"
                SELECT seq, content_json FROM messages
                WHERE session_id = ?1 AND agent_id = ?2 AND seq < ?3
                ORDER BY seq DESC
                LIMIT ?4;
                "#,
            )?;
            let mapped = stmt.query_map(
                params![session.as_str(), agent.as_str(), before_seq as i64, limit],
                |row| {
                    let seq: i64 = row.get(0)?;
                    let json: String = row.get(1)?;
                    Ok((seq, json))
                },
            )?;
            let mut res = Vec::new();
            for r in mapped {
                let (seq, json) = r?;
                let message: Message = serde_json::from_str(&json)?;
                res.push(MessageRow {
                    seq: seq as u64,
                    message,
                });
            }
            res
        }
        None => {
            let mut stmt = conn.prepare_cached(
                r#"
                SELECT seq, content_json FROM messages
                WHERE session_id = ?1 AND agent_id = ?2
                ORDER BY seq DESC
                LIMIT ?3;
                "#,
            )?;
            let mapped =
                stmt.query_map(params![session.as_str(), agent.as_str(), limit], |row| {
                    let seq: i64 = row.get(0)?;
                    let json: String = row.get(1)?;
                    Ok((seq, json))
                })?;
            let mut res = Vec::new();
            for r in mapped {
                let (seq, json) = r?;
                let message: Message = serde_json::from_str(&json)?;
                res.push(MessageRow {
                    seq: seq as u64,
                    message,
                });
            }
            res
        }
    };
    rows.reverse();
    Ok(rows)
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

fn fallback_snippet(content_json: &str, query: &str) -> String {
    let plain = extract_message_plain_text(content_json);
    if plain.is_empty() {
        return String::new();
    }
    let term = query.split_whitespace().next().unwrap_or(query);
    if let Some((start, end)) = find_case_insensitive(&plain, term) {
        let matched = &plain[start..end];
        let prefix = &plain[..start];
        let suffix = &plain[end..];

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
        let words: Vec<&str> = plain.split_whitespace().take(12).collect();
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
        SELECT m.session_id, m.agent_id, m.seq, snippet(messages_fts, 0, '[', ']', '…', 12), m.content_json
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
        let content_json: String = row.get(4)?;
        Ok((session, agent, seq, raw_snippet, content_json))
    })?;

    let mut hits = Vec::new();
    for row in rows {
        let (session, agent, seq, raw_snippet, content_json) = row?;
        let snippet = match raw_snippet {
            Some(s) if !s.is_empty() => s,
            _ => fallback_snippet(&content_json, trimmed),
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
