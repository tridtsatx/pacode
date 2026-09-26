//! State queries for `Store`: agents, tasks, plans, usage, compaction.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use pacode_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, Effort, Message, ModelRoute, Plan, SessionId,
    TaskId, TaskInfo, TaskProgress, TaskStatus, Usage, UsageTotals,
};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;
use crate::txn::Txn;

pub(crate) fn agent_kind_to_str(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Main => "main",
        AgentKind::Sub => "sub",
    }
}

pub(crate) fn agent_kind_from_str(s: &str) -> AgentKind {
    match s {
        "main" => AgentKind::Main,
        _ => AgentKind::Sub,
    }
}

pub(crate) fn agent_status_to_str(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Thinking => "thinking",
        AgentStatus::RunningTool => "running_tool",
        AgentStatus::WaitingApproval => "waiting_approval",
        AgentStatus::Finished => "finished",
        AgentStatus::Failed => "failed",
        AgentStatus::Stopped => "stopped",
    }
}

pub(crate) fn agent_status_from_str(s: &str) -> AgentStatus {
    match s {
        "idle" => AgentStatus::Idle,
        "thinking" => AgentStatus::Thinking,
        "running_tool" => AgentStatus::RunningTool,
        "waiting_approval" => AgentStatus::WaitingApproval,
        "finished" => AgentStatus::Finished,
        "failed" => AgentStatus::Failed,
        "stopped" => AgentStatus::Stopped,
        _ => AgentStatus::Idle,
    }
}

pub(crate) fn task_status_to_str(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Killed => "killed",
    }
}

pub(crate) fn task_status_from_str(s: &str) -> TaskStatus {
    match s {
        "running" => TaskStatus::Running,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "killed" => TaskStatus::Killed,
        _ => TaskStatus::Running,
    }
}

/// A JSON array of `seq`, so detach predicates stay one statement per table
/// instead of one statement per message.
fn seq_set_json(seqs: &BTreeSet<u64>) -> String {
    let mut json = String::with_capacity(seqs.len() * 4 + 2);
    json.push('[');
    for (index, seq) in seqs.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&seq.to_string());
    }
    json.push(']');
    json
}

/// Rows past the compaction summary that the fresh agent did not retain, so they
/// belong to the detached agent. `seq` is never null in `messages`, while an
/// admission without a `message_seq` cannot be placed on either side of the
/// handover and stays with the old owner.
fn moved_seq_predicate(column: &str, summary_index: usize, retained_index: usize) -> String {
    format!(
        "(?{summary_index} IS NULL OR {column} > ?{summary_index}) \
         AND {column} NOT IN (SELECT value FROM json_each(?{retained_index}))"
    )
}

fn moved_message_predicate() -> String {
    format!(
        "session_id = ?1 AND agent_id = ?2 AND {}",
        moved_seq_predicate("seq", 3, 4)
    )
}

fn moved_admission_predicate() -> String {
    format!(
        "session_id = ?2 AND agent_id = ?3 AND message_seq IS NOT NULL AND {}",
        moved_seq_predicate("message_seq", 4, 5)
    )
}

pub fn upsert_agent(
    conn: &mut Connection,
    session: &SessionId,
    info: &AgentInfo,
    prompt: Option<&str>,
) -> Result<(), StoreError> {
    let tx = Txn::open(conn)?;
    upsert_agent_in_unit_of_work(&tx, session, info, prompt)?;
    tx.commit()?;
    Ok(())
}

pub fn create_agent_with_history(
    conn: &mut Connection,
    session: &SessionId,
    info: &AgentInfo,
    prompt: Option<&str>,
    rows: &[(u64, Arc<Message>)],
) -> Result<(), StoreError> {
    let tx = Txn::open(conn)?;
    upsert_agent_in_unit_of_work(&tx, session, info, prompt)?;
    for (seq, message) in rows {
        super::message::append_message_in_unit_of_work(
            &tx,
            session,
            &info.id,
            *seq,
            message.as_ref(),
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Upserts inside a unit of work the caller already owns, for the same reason
/// [`super::message::append_message_in_unit_of_work`] does not open one.
pub(crate) fn upsert_agent_in_unit_of_work(
    conn: &Connection,
    session: &SessionId,
    info: &AgentInfo,
    prompt: Option<&str>,
) -> Result<(), StoreError> {
    let kind = agent_kind_to_str(info.kind);
    let status = agent_status_to_str(info.status);
    let model = format!("{}/{}", info.model.provider, info.model.model);
    let parent = info.parent.as_ref().map(|p| p.as_str());

    conn.execute(
        r#"
        INSERT INTO agents (
            id, session_id, name, kind, status, activity, parent_id, prompt,
            model, effort, started_at, finished_at, tokens_in, tokens_out, summary, error
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        ON CONFLICT(session_id, id) DO UPDATE SET
            name = excluded.name,
            kind = excluded.kind,
            status = excluded.status,
            activity = excluded.activity,
            parent_id = excluded.parent_id,
            prompt = COALESCE(excluded.prompt, agents.prompt),
            model = excluded.model,
            effort = excluded.effort,
            started_at = excluded.started_at,
            finished_at = excluded.finished_at,
            tokens_in = excluded.tokens_in,
            tokens_out = excluded.tokens_out,
            summary = excluded.summary,
            error = excluded.error;
        "#,
        params![
            info.id.as_str(),
            session.as_str(),
            info.name.as_str(),
            kind,
            status,
            info.activity.as_deref(),
            parent,
            prompt,
            model,
            info.effort.as_str(),
            info.started_at_ms as i64,
            info.finished_at_ms.map(|t| t as i64),
            info.tokens_in as i64,
            info.tokens_out as i64,
            info.summary.as_deref(),
            info.error.as_deref(),
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn detach_agent(
    conn: &mut Connection,
    session: &SessionId,
    info: &AgentInfo,
    prompt: Option<&str>,
    rows: &[(u64, Arc<Message>)],
    retained_seqs: &[u64],
    summary_upto: Option<u64>,
    old_owner: &AgentId,
    new_owner: &AgentId,
) -> Result<(), StoreError> {
    let tx = Txn::open(conn)?;
    upsert_agent_in_unit_of_work(&tx, session, info, prompt)?;

    let retained_seqs = retained_seqs.iter().copied().collect::<BTreeSet<_>>();
    let retained_json = seq_set_json(&retained_seqs);
    let summary_upto = summary_upto.map(|upto| upto as i64);

    super::message::purge_messages(
        &tx,
        &moved_message_predicate(),
        &[
            Value::Text(session.as_str().to_string()),
            Value::Text(old_owner.as_str().to_string()),
            summary_upto.map_or(Value::Null, Value::Integer),
            Value::Text(retained_json.clone()),
        ],
    )?;

    tx.execute(
        &format!(
            "UPDATE message_admissions SET agent_id = ?1 WHERE {};",
            moved_admission_predicate()
        ),
        params![
            new_owner.as_str(),
            session.as_str(),
            old_owner.as_str(),
            summary_upto,
            retained_json,
        ],
    )?;

    for (seq, message) in rows {
        super::message::append_message_in_unit_of_work(
            &tx,
            session,
            &info.id,
            *seq,
            message.as_ref(),
        )?;
    }

    if let Some((summary, upto_seq)) = tx
        .query_row(
            "SELECT summary, upto_seq FROM compaction WHERE session_id = ?1 AND agent_id = ?2;",
            params![session.as_str(), old_owner.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
    {
        tx.execute(
            "INSERT INTO compaction (session_id, agent_id, summary, upto_seq) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(session_id, agent_id) DO UPDATE SET summary = excluded.summary, upto_seq = excluded.upto_seq;",
            params![session.as_str(), new_owner.as_str(), summary, upto_seq],
        )?;
    }
    tx.execute(
        "UPDATE tasks SET owner_agent = ?1 WHERE session_id = ?2 AND owner_agent = ?3;",
        params![new_owner.as_str(), session.as_str(), old_owner.as_str()],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn rollback_detach(
    conn: &mut Connection,
    session: &SessionId,
    detached: &AgentId,
    rows: &[(u64, Arc<Message>)],
    old_owner: &AgentId,
) -> Result<(), StoreError> {
    let tx = Txn::open(conn)?;
    super::message::purge_messages(
        &tx,
        "session_id = ?1 AND agent_id = ?2",
        &[
            Value::Text(session.as_str().to_string()),
            Value::Text(detached.as_str().to_string()),
        ],
    )?;
    tx.execute(
        "DELETE FROM agents WHERE session_id = ?1 AND id = ?2;",
        params![session.as_str(), detached.as_str()],
    )?;
    tx.execute(
        "DELETE FROM compaction WHERE session_id = ?1 AND agent_id = ?2;",
        params![session.as_str(), detached.as_str()],
    )?;
    for (seq, message) in rows {
        super::message::append_message_in_unit_of_work(
            &tx,
            session,
            old_owner,
            *seq,
            message.as_ref(),
        )?;
    }
    tx.execute(
        "UPDATE message_admissions SET agent_id = ?1 WHERE session_id = ?2 AND agent_id = ?3;",
        params![old_owner.as_str(), session.as_str(), detached.as_str()],
    )?;
    tx.execute(
        "UPDATE tasks SET owner_agent = ?1 WHERE session_id = ?2 AND owner_agent = ?3;",
        params![old_owner.as_str(), session.as_str(), detached.as_str()],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn list_agents(conn: &Connection, session: &SessionId) -> Result<Vec<AgentInfo>, StoreError> {
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT id, name, kind, status, activity, parent_id, model, effort, started_at, finished_at, tokens_in, tokens_out, summary, error
        FROM agents
        WHERE session_id = ?1
        ORDER BY started_at ASC, id ASC;
        "#,
    )?;
    let rows = stmt.query_map(params![session.as_str()], |row| {
        let id_str: String = row.get(0)?;
        let name: String = row.get(1)?;
        let kind_str: String = row.get(2)?;
        let status_str: String = row.get(3)?;
        let activity: Option<String> = row.get(4)?;
        let parent_str: Option<String> = row.get(5)?;
        let model_str: String = row.get(6)?;
        let effort_str: String = row.get(7)?;
        let started_at: i64 = row.get(8)?;
        let finished_at: Option<i64> = row.get(9)?;
        let tokens_in: i64 = row.get(10)?;
        let tokens_out: i64 = row.get(11)?;
        let summary: Option<String> = row.get(12)?;
        let error: Option<String> = row.get(13)?;

        let model = ModelRoute::parse_lossy(&model_str)
            .unwrap_or_else(|| ModelRoute::new("unknown", model_str));
        let effort = Effort::parse(&effort_str).unwrap_or_default();
        let kind = agent_kind_from_str(&kind_str);
        let status = agent_status_from_str(&status_str);

        Ok(AgentInfo {
            id: AgentId::new(id_str),
            name,
            kind,
            status,
            activity,
            parent: parent_str.map(AgentId::new),
            started_at_ms: started_at as u64,
            finished_at_ms: finished_at.map(|t| t as u64),
            tokens_in: tokens_in as u64,
            tokens_out: tokens_out as u64,
            model,
            effort,
            summary,
            error,
        })
    })?;

    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}

pub fn agent_prompt(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
) -> Result<Option<String>, StoreError> {
    let mut stmt =
        conn.prepare_cached("SELECT prompt FROM agents WHERE session_id = ?1 AND id = ?2;")?;
    Ok(stmt
        .query_row(params![session.as_str(), agent.as_str()], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

pub fn upsert_task(conn: &mut Connection, info: &TaskInfo) -> Result<(), StoreError> {
    upsert_task_checked(conn, info).map(|_| ())
}

pub fn upsert_task_checked(conn: &mut Connection, info: &TaskInfo) -> Result<bool, StoreError> {
    let cwd_str = info.cwd.to_string_lossy().to_string();
    let output_str = info.output_path.to_string_lossy().to_string();
    let status = task_status_to_str(info.status);
    let backgrounded = if info.backgrounded { 1 } else { 0 };
    let acked = if info.acked { 1 } else { 0 };
    let progress_json = info
        .progress
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    let changed = conn.execute(
        r#"
        INSERT INTO tasks (
            id, session_id, owner_agent, label, command, cwd, status,
            backgrounded, exit_code, started_at, ended_at, output_path, output_bytes,
            progress_json, warnings, errors, acked
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
        ON CONFLICT(id) DO UPDATE SET
            session_id = excluded.session_id,
            owner_agent = excluded.owner_agent,
            label = excluded.label,
            command = excluded.command,
            cwd = excluded.cwd,
            status = excluded.status,
            backgrounded = excluded.backgrounded,
            exit_code = excluded.exit_code,
            started_at = excluded.started_at,
            ended_at = excluded.ended_at,
            output_path = excluded.output_path,
            output_bytes = excluded.output_bytes,
            progress_json = excluded.progress_json,
            warnings = excluded.warnings,
            errors = excluded.errors,
            acked = excluded.acked
        WHERE tasks.session_id = excluded.session_id
          AND tasks.owner_agent = excluded.owner_agent
          AND (
              tasks.status NOT IN ('completed', 'failed', 'killed')
              OR excluded.status = tasks.status
          );
        "#,
        params![
            info.id.as_str(),
            info.session.as_str(),
            info.owner.as_str(),
            info.label.as_str(),
            info.command.as_str(),
            cwd_str,
            status,
            backgrounded,
            info.exit_code,
            info.started_at_ms as i64,
            info.ended_at_ms.map(|t| t as i64),
            output_str,
            info.output_bytes as i64,
            progress_json,
            info.warnings as i64,
            info.errors as i64,
            acked,
        ],
    )?;
    Ok(changed == 1)
}

const TASK_COLUMNS: &str = "id, session_id, owner_agent, label, command, cwd, status, \
                           backgrounded, exit_code, started_at, ended_at, output_path, \
                           output_bytes, progress_json, warnings, errors, acked";

pub fn reassign_task_owner(
    conn: &Connection,
    session: &SessionId,
    from: &AgentId,
    to: &AgentId,
) -> Result<Vec<TaskInfo>, StoreError> {
    let mut stmt = conn.prepare_cached(&format!(
        r#"
        UPDATE tasks SET owner_agent = ?1
        WHERE session_id = ?2 AND owner_agent = ?3
        RETURNING {TASK_COLUMNS};
        "#
    ))?;
    let rows = stmt.query_map(
        params![to.as_str(), session.as_str(), from.as_str()],
        task_info_from_row,
    )?;
    let mut changed = Vec::new();
    for row in rows {
        changed.push(row?);
    }
    Ok(changed)
}

fn task_info_from_row(row: &rusqlite::Row) -> Result<TaskInfo, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let session_str: String = row.get(1)?;
    let owner_str: String = row.get(2)?;
    let label: String = row.get(3)?;
    let command: String = row.get(4)?;
    let cwd_str: String = row.get(5)?;
    let status_str: String = row.get(6)?;
    let backgrounded: i64 = row.get(7)?;
    let exit_code: Option<i32> = row.get(8)?;
    let started_at: i64 = row.get(9)?;
    let ended_at: Option<i64> = row.get(10)?;
    let output_str: String = row.get(11)?;
    let output_bytes: i64 = row.get(12)?;
    let progress_str: Option<String> = row.get(13)?;
    let warnings: i64 = row.get(14)?;
    let errors: i64 = row.get(15)?;
    let acked: i64 = row.get(16)?;

    let progress = match progress_str {
        Some(text) => serde_json::from_str::<TaskProgress>(&text).ok(),
        None => None,
    };

    Ok(TaskInfo {
        id: TaskId::new(id_str),
        session: SessionId::new(session_str),
        owner: AgentId::new(owner_str),
        label,
        command,
        cwd: PathBuf::from(cwd_str),
        status: task_status_from_str(&status_str),
        backgrounded: backgrounded != 0,
        exit_code,
        started_at_ms: started_at as u64,
        ended_at_ms: ended_at.map(|value| value as u64),
        progress,
        warnings: warnings as u32,
        errors: errors as u32,
        output_path: PathBuf::from(output_str),
        output_bytes: output_bytes as u64,
        acked: acked != 0,
    })
}

pub fn list_tasks(conn: &Connection, session: &SessionId) -> Result<Vec<TaskInfo>, StoreError> {
    let mut stmt = conn.prepare_cached(&format!(
        r#"
        SELECT {TASK_COLUMNS}
        FROM tasks
        WHERE session_id = ?1
        ORDER BY started_at ASC, id ASC;
        "#
    ))?;

    let rows = stmt.query_map(params![session.as_str()], task_info_from_row)?;

    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}

pub fn save_plan(
    conn: &mut Connection,
    session: &SessionId,
    plan: &Plan,
) -> Result<(), StoreError> {
    let json = serde_json::to_string(plan)?;
    conn.execute(
        r#"
        INSERT INTO plans (session_id, version, json)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(session_id) DO UPDATE SET
            version = excluded.version,
            json = excluded.json;
        "#,
        params![session.as_str(), plan.version as i64, json],
    )?;
    Ok(())
}

pub fn load_plan(conn: &Connection, session: &SessionId) -> Result<Plan, StoreError> {
    let mut stmt = conn.prepare_cached("SELECT json FROM plans WHERE session_id = ?1;")?;
    let mut rows = stmt.query(params![session.as_str()])?;
    if let Some(row) = rows.next()? {
        let json: String = row.get(0)?;
        let plan: Plan = serde_json::from_str(&json)?;
        Ok(plan)
    } else {
        Ok(Plan::default())
    }
}

pub fn add_usage(
    conn: &mut Connection,
    session: &SessionId,
    agent: &AgentId,
    turn: u32,
    usage: &Usage,
    cost_usd: Option<f64>,
) -> Result<(), StoreError> {
    add_usage_for_turn(conn, session, agent, turn, None, usage, cost_usd).map(|_| ())
}

pub fn add_usage_for_turn(
    conn: &mut Connection,
    session: &SessionId,
    agent: &AgentId,
    turn: u32,
    turn_id: Option<&str>,
    usage: &Usage,
    cost_usd: Option<f64>,
) -> Result<bool, StoreError> {
    let now = pacode_types::time::now_ms() as i64;
    let changed = conn.execute(
        r#"
        INSERT INTO usage (session_id, agent_id, turn, turn_id, input, output, reasoning, cache_read, cache_write, cost_usd, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT DO NOTHING;
        "#,
        params![
            session.as_str(),
            agent.as_str(),
            turn as i64,
            turn_id,
            usage.input_tokens as i64,
            usage.output_tokens as i64,
            usage.reasoning_tokens as i64,
            usage.cache_read_tokens as i64,
            usage.cache_write_tokens as i64,
            cost_usd,
            now,
        ],
    )?;
    Ok(changed == 1)
}

pub fn usage_totals(conn: &Connection, session: &SessionId) -> Result<UsageTotals, StoreError> {
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT
            COALESCE(SUM(input), 0),
            COALESCE(SUM(output), 0),
            COALESCE(SUM(reasoning), 0),
            COALESCE(SUM(cache_read), 0),
            COALESCE(SUM(cache_write), 0),
            COUNT(DISTINCT CASE
                WHEN turn_id IS NULL THEN 'legacy:' || agent_id || ':' || turn
                ELSE turn_id
            END),
            SUM(cost_usd),
            COUNT(cost_usd),
            COALESCE(MIN(created_at), 0),
            COALESCE(MAX(created_at), 0)
        FROM usage
        WHERE session_id = ?1;
        "#,
    )?;

    let totals = stmt.query_row(params![session.as_str()], |row| {
        let input: i64 = row.get(0)?;
        let output: i64 = row.get(1)?;
        let reasoning: i64 = row.get(2)?;
        let cache_read: i64 = row.get(3)?;
        let cache_write: i64 = row.get(4)?;
        let turns: i64 = row.get(5)?;
        let sum_cost: Option<f64> = row.get(6)?;
        let cost_count: i64 = row.get(7)?;
        let started_at: i64 = row.get(8)?;
        let last_activity: i64 = row.get(9)?;

        let cost_usd = if cost_count > 0 { sum_cost } else { None };

        Ok(UsageTotals {
            input: input as u64,
            output: output as u64,
            reasoning: reasoning as u64,
            cache_read: cache_read as u64,
            cache_write: cache_write as u64,
            turns: turns as u32,
            cost_usd,
            context_tokens: 0,
            context_window: None,
            started_at_ms: started_at as u64,
            last_activity_ms: last_activity as u64,
        })
    })?;

    Ok(totals)
}

pub fn save_compaction(
    conn: &mut Connection,
    session: &SessionId,
    agent: &AgentId,
    summary: &str,
    upto_seq: u64,
) -> Result<(), StoreError> {
    conn.execute(
        r#"
        INSERT INTO compaction (session_id, agent_id, summary, upto_seq)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(session_id, agent_id) DO UPDATE SET
            summary = excluded.summary,
            upto_seq = excluded.upto_seq;
        "#,
        params![session.as_str(), agent.as_str(), summary, upto_seq as i64],
    )?;
    Ok(())
}

pub fn load_compaction(
    conn: &Connection,
    session: &SessionId,
    agent: &AgentId,
) -> Result<Option<(String, u64)>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT summary, upto_seq FROM compaction WHERE session_id = ?1 AND agent_id = ?2;",
    )?;
    let mut rows = stmt.query(params![session.as_str(), agent.as_str()])?;
    if let Some(row) = rows.next()? {
        let summary: String = row.get(0)?;
        let upto_seq: i64 = row.get(1)?;
        Ok(Some((summary, upto_seq as u64)))
    } else {
        Ok(None)
    }
}
