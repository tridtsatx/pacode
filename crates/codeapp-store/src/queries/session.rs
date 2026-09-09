//! Session queries for `Store`.

use std::path::PathBuf;

use codeapp_types::{Effort, Mode, ModelRoute, SessionId, SessionMeta};
use rusqlite::{Connection, params};

use super::SessionFilter;
use crate::StoreError;
use crate::queries::message::extract_message_plain_text;

pub fn upsert_session(conn: &mut Connection, meta: &SessionMeta) -> Result<(), StoreError> {
    let cwd_str = meta.cwd.to_string_lossy().to_string();
    let model_str = format!("{}/{}", meta.model.provider, meta.model.model);
    conn.execute(
        r#"
        INSERT INTO sessions (id, name, cwd, git_branch, created_at, updated_at, model, effort, mode, first_prompt, meta_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            cwd = excluded.cwd,
            git_branch = excluded.git_branch,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            model = excluded.model,
            effort = excluded.effort,
            mode = excluded.mode,
            first_prompt = excluded.first_prompt,
            meta_json = excluded.meta_json;
        "#,
        params![
            meta.id.as_str(),
            meta.name.as_deref(),
            cwd_str,
            meta.git_branch.as_deref(),
            meta.created_at_ms as i64,
            meta.updated_at_ms as i64,
            model_str,
            meta.effort.as_str(),
            meta.mode.as_str(),
            meta.first_prompt.as_deref(),
            None::<&str>,
        ],
    )?;
    Ok(())
}

fn row_to_session_meta(row: &rusqlite::Row) -> Result<SessionMeta, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let name: Option<String> = row.get(1)?;
    let cwd_str: String = row.get(2)?;
    let git_branch: Option<String> = row.get(3)?;
    let created_at: i64 = row.get(4)?;
    let updated_at: i64 = row.get(5)?;
    let model_str: String = row.get(6)?;
    let effort_str: String = row.get(7)?;
    let mode_str: String = row.get(8)?;
    let first_prompt: Option<String> = row.get(9)?;

    let model = ModelRoute::parse_lossy(&model_str)
        .unwrap_or_else(|| ModelRoute::new("unknown", model_str));
    let effort = Effort::parse(&effort_str).unwrap_or_default();
    let mode = Mode::parse(&mode_str).unwrap_or_default();

    Ok(SessionMeta {
        id: SessionId::new(id_str),
        name,
        cwd: PathBuf::from(cwd_str),
        git_branch,
        created_at_ms: created_at as u64,
        updated_at_ms: updated_at as u64,
        model,
        effort,
        mode,
        first_prompt,
    })
}

pub fn get_session(conn: &Connection, id: &SessionId) -> Result<Option<SessionMeta>, StoreError> {
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT id, name, cwd, git_branch, created_at, updated_at, model, effort, mode, first_prompt
        FROM sessions
        WHERE id = ?1;
        "#,
    )?;
    let mut rows = stmt.query(params![id.as_str()])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row_to_session_meta(row)?))
    } else {
        Ok(None)
    }
}

pub fn list_sessions(
    conn: &Connection,
    filter: SessionFilter,
) -> Result<Vec<SessionMeta>, StoreError> {
    let limit = if filter.limit == 0 { 200 } else { filter.limit };
    let mut result = Vec::new();
    match filter.cwd {
        Some(cwd) => {
            let cwd_str = cwd.to_string_lossy().to_string();
            let mut stmt = conn.prepare_cached(
                r#"
                SELECT id, name, cwd, git_branch, created_at, updated_at, model, effort, mode, first_prompt
                FROM sessions
                WHERE cwd = ?1
                ORDER BY updated_at DESC
                LIMIT ?2;
                "#,
            )?;
            let mut rows = stmt.query(params![cwd_str, limit])?;
            while let Some(row) = rows.next()? {
                result.push(row_to_session_meta(row)?);
            }
        }
        None => {
            let mut stmt = conn.prepare_cached(
                r#"
                SELECT id, name, cwd, git_branch, created_at, updated_at, model, effort, mode, first_prompt
                FROM sessions
                ORDER BY updated_at DESC
                LIMIT ?1;
                "#,
            )?;
            let mut rows = stmt.query(params![limit])?;
            while let Some(row) = rows.next()? {
                result.push(row_to_session_meta(row)?);
            }
        }
    }
    Ok(result)
}

pub fn delete_session(conn: &mut Connection, id: &SessionId) -> Result<(), StoreError> {
    let tx = conn.transaction()?;
    {
        let mut stmt =
            tx.prepare_cached("SELECT id, content_json FROM messages WHERE session_id = ?1;")?;
        let rows = stmt.query_map(params![id.as_str()], |row| {
            let mid: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            Ok((mid, json))
        })?;
        let mut to_delete = Vec::new();
        for r in rows {
            let (mid, json) = r?;
            to_delete.push((mid, extract_message_plain_text(&json)));
        }
        drop(stmt);

        let mut fts_del = tx.prepare_cached(
            "INSERT INTO messages_fts(messages_fts, rowid, text) VALUES('delete', ?1, ?2);",
        )?;
        for (mid, text) in to_delete {
            fts_del.execute(params![mid, text])?;
        }
        drop(fts_del);

        tx.execute("DELETE FROM sessions WHERE id = ?1;", params![id.as_str()])?;
    }
    tx.commit()?;
    Ok(())
}
