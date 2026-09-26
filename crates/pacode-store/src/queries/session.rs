//! Session queries for `Store`.

use std::path::PathBuf;

use pacode_types::{AgentId, ClientId, Effort, Mode, ModelRoute, SessionId, SessionMeta, TaskId};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use super::{
    AttachAdmission, AttachAdmissionResult, SessionCursor, SessionFilter, SessionPage,
    TaskAdmission, TaskAdmissionResult,
};
use crate::StoreError;
use crate::queries::message::purge_messages;
use crate::txn::Txn;

pub const DEFAULT_SESSION_PAGE: u32 = 200;

const SESSION_COLUMNS: &str =
    "id, name, cwd, git_branch, created_at, updated_at, model, effort, mode, first_prompt, zdr";

pub fn upsert_session(conn: &mut Connection, meta: &SessionMeta) -> Result<(), StoreError> {
    let cwd_str = meta.cwd.to_string_lossy().to_string();
    let model_str = format!("{}/{}", meta.model.provider, meta.model.model);
    conn.execute(
        r#"
        INSERT INTO sessions (id, name, cwd, git_branch, created_at, updated_at, model, effort, mode, first_prompt, meta_json, zdr)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
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
            meta_json = excluded.meta_json,
            zdr = excluded.zdr;
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
            meta.zdr,
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
    let zdr: bool = row.get(10)?;

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
        zdr,
    })
}

pub fn get_session(conn: &Connection, id: &SessionId) -> Result<Option<SessionMeta>, StoreError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1;"
    ))?;
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
    Ok(list_sessions_page(conn, &filter, None)?.sessions)
}

/// One keyset page of the `(updated_at DESC, id ASC)` order, so equal timestamps
/// never reshuffle between pages and no session is skipped or repeated.
pub fn list_sessions_page(
    conn: &Connection,
    filter: &SessionFilter,
    cursor: Option<&SessionCursor>,
) -> Result<SessionPage, StoreError> {
    let page = if filter.limit == 0 {
        DEFAULT_SESSION_PAGE
    } else {
        filter.limit
    };
    let cap = page as i64;

    let mut filters: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();
    if let Some(cwd) = &filter.cwd {
        args.push(Value::Text(cwd.to_string_lossy().into_owned()));
        filters.push(format!("cwd = ?{}", args.len()));
    }
    if let Some(cursor) = cursor {
        let updated_index = args.len() + 1;
        let id_index = updated_index + 1;
        args.push(Value::Integer(cursor.updated_at_ms as i64));
        args.push(Value::Text(cursor.id.as_str().to_string()));
        filters.push(format!(
            "(updated_at < ?{updated_index} OR (updated_at = ?{updated_index} AND id > ?{id_index}))"
        ));
    }
    args.push(Value::Integer(cap + 1));

    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM sessions WHERE {} ORDER BY updated_at DESC, id ASC LIMIT ?{};",
        if filters.is_empty() {
            "1 = 1".to_string()
        } else {
            filters.join(" AND ")
        },
        args.len()
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), row_to_session_meta)?;

    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row?);
    }
    let has_more = sessions.len() as i64 > cap;
    if has_more {
        sessions.truncate(cap as usize);
    }
    let next_cursor = match sessions.last() {
        Some(meta) if has_more => Some(SessionCursor {
            updated_at_ms: meta.updated_at_ms,
            id: meta.id.clone(),
        }),
        _ => None,
    };
    Ok(SessionPage {
        sessions,
        next_cursor,
    })
}

fn row_to_attach_admission(row: &rusqlite::Row) -> Result<AttachAdmission, rusqlite::Error> {
    let client_id: String = row.get(0)?;
    let idempotency_key: String = row.get(1)?;
    let session_id: String = row.get(2)?;
    let fingerprint: String = row.get(3)?;
    let allow_create: i64 = row.get(4)?;
    Ok(AttachAdmission {
        client_id: ClientId::new(client_id),
        idempotency_key,
        session_id: SessionId::new(session_id),
        fingerprint,
        allow_create: allow_create != 0,
    })
}

pub fn get_attach_admission(
    conn: &Connection,
    client_id: &ClientId,
    idempotency_key: &str,
) -> Result<Option<AttachAdmission>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT client_id, idempotency_key, session_id, fingerprint, allow_create FROM attach_admissions WHERE client_id = ?1 AND idempotency_key = ?2;",
    )?;
    let row = stmt
        .query_row(
            params![client_id.as_str(), idempotency_key],
            row_to_attach_admission,
        )
        .optional()?;
    Ok(row)
}

pub fn admit_attach(
    conn: &mut Connection,
    client_id: &ClientId,
    idempotency_key: &str,
    fingerprint: &str,
    session_id: &SessionId,
    allow_create: bool,
) -> Result<AttachAdmissionResult, StoreError> {
    let tx = Txn::open(conn)?;
    let inserted = tx.execute(
        "INSERT INTO attach_admissions (client_id, idempotency_key, session_id, fingerprint, allow_create, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(client_id, idempotency_key) DO NOTHING;",
        params![
            client_id.as_str(),
            idempotency_key,
            session_id.as_str(),
            fingerprint,
            allow_create as i64,
            pacode_types::now_ms() as i64,
        ],
    )?;
    let admission = tx.query_row(
        "SELECT client_id, idempotency_key, session_id, fingerprint, allow_create FROM attach_admissions WHERE client_id = ?1 AND idempotency_key = ?2;",
        params![client_id.as_str(), idempotency_key],
        row_to_attach_admission,
    )?;
    let result = if inserted == 1 {
        AttachAdmissionResult::Admitted(admission)
    } else if admission.fingerprint == fingerprint {
        AttachAdmissionResult::Duplicate(admission)
    } else {
        AttachAdmissionResult::Conflict(admission)
    };
    tx.commit()?;
    Ok(result)
}

fn row_to_task_admission(row: &rusqlite::Row) -> Result<TaskAdmission, rusqlite::Error> {
    let session: String = row.get(0)?;
    let idempotency_key: String = row.get(1)?;
    let task_id: String = row.get(2)?;
    let owner: String = row.get(3)?;
    let fingerprint: String = row.get(4)?;
    let created_at: i64 = row.get(5)?;
    Ok(TaskAdmission {
        session: SessionId::new(session),
        idempotency_key,
        task_id: TaskId::new(task_id),
        owner: AgentId::new(owner),
        fingerprint,
        created_at_ms: created_at as u64,
    })
}

const TASK_ADMISSION_COLUMNS: &str =
    "session_id, idempotency_key, task_id, owner_agent, fingerprint, created_at";

pub fn get_task_admission(
    conn: &Connection,
    session: &SessionId,
    idempotency_key: &str,
) -> Result<Option<TaskAdmission>, StoreError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {TASK_ADMISSION_COLUMNS} FROM task_admissions WHERE session_id = ?1 AND idempotency_key = ?2;"
    ))?;
    let row = stmt
        .query_row(
            params![session.as_str(), idempotency_key],
            row_to_task_admission,
        )
        .optional()?;
    Ok(row)
}

pub fn get_task_admission_by_task(
    conn: &Connection,
    task_id: &TaskId,
) -> Result<Option<TaskAdmission>, StoreError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {TASK_ADMISSION_COLUMNS} FROM task_admissions WHERE task_id = ?1;"
    ))?;
    let row = stmt
        .query_row(params![task_id.as_str()], row_to_task_admission)
        .optional()?;
    Ok(row)
}

/// Reserves `task_id` for one logical start request. The caller must spawn the
/// process only on [`TaskAdmissionResult::Admitted`]: a duplicate hands back the
/// id the first start already owns, so a replayed request can never leave two
/// processes behind.
pub fn admit_task(
    conn: &mut Connection,
    session: &SessionId,
    idempotency_key: &str,
    fingerprint: &str,
    task_id: &TaskId,
    owner: &AgentId,
) -> Result<TaskAdmissionResult, StoreError> {
    let tx = Txn::open(conn)?;
    let inserted = tx.execute(
        "INSERT INTO task_admissions (session_id, idempotency_key, task_id, owner_agent, fingerprint, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(session_id, idempotency_key) DO NOTHING;",
        params![
            session.as_str(),
            idempotency_key,
            task_id.as_str(),
            owner.as_str(),
            fingerprint,
            pacode_types::now_ms() as i64,
        ],
    )?;
    let admission = tx.query_row(
        &format!(
            "SELECT {TASK_ADMISSION_COLUMNS} FROM task_admissions WHERE session_id = ?1 AND idempotency_key = ?2;"
        ),
        params![session.as_str(), idempotency_key],
        row_to_task_admission,
    )?;
    let result = if inserted == 1 {
        TaskAdmissionResult::Admitted(admission)
    } else if admission.fingerprint == fingerprint {
        TaskAdmissionResult::Duplicate(admission)
    } else {
        TaskAdmissionResult::Conflict(admission)
    };
    tx.commit()?;
    Ok(result)
}

pub fn delete_session(conn: &mut Connection, id: &SessionId) -> Result<(), StoreError> {
    let tx = Txn::open(conn)?;
    purge_messages(
        &tx,
        "session_id = ?1",
        &[Value::Text(id.as_str().to_string())],
    )?;
    tx.execute("DELETE FROM sessions WHERE id = ?1;", params![id.as_str()])?;
    tx.commit()?;
    Ok(())
}
