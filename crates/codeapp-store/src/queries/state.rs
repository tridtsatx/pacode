//! State queries for `Store`: agents, tasks, plans, usage, compaction.

use std::path::PathBuf;

use codeapp_types::{
    AgentId, AgentInfo, AgentKind, AgentStatus, Effort, ModelRoute, Plan, SessionId, TaskId,
    TaskInfo, TaskProgress, TaskStatus, Usage, UsageTotals,
};
use rusqlite::{Connection, params};

use crate::StoreError;

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

pub fn upsert_agent(
    conn: &mut Connection,
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
        ON CONFLICT(id) DO UPDATE SET
            session_id = excluded.session_id,
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

pub fn list_agents(conn: &Connection, session: &SessionId) -> Result<Vec<AgentInfo>, StoreError> {
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT id, name, kind, status, activity, parent_id, model, effort, started_at, finished_at, tokens_in, tokens_out, summary, error
        FROM agents
        WHERE session_id = ?1
        ORDER BY started_at ASC;
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

pub fn upsert_task(conn: &mut Connection, info: &TaskInfo) -> Result<(), StoreError> {
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

    conn.execute(
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
            acked = excluded.acked;
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
    Ok(())
}

pub fn list_tasks(conn: &Connection, session: &SessionId) -> Result<Vec<TaskInfo>, StoreError> {
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT id, session_id, owner_agent, label, command, cwd, status,
               backgrounded, exit_code, started_at, ended_at, output_path, output_bytes,
               progress_json, warnings, errors, acked
        FROM tasks
        WHERE session_id = ?1
        ORDER BY started_at ASC;
        "#,
    )?;

    let rows = stmt.query_map(params![session.as_str()], |row| {
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
            Some(s) => serde_json::from_str::<TaskProgress>(&s).ok(),
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
            ended_at_ms: ended_at.map(|t| t as u64),
            progress,
            warnings: warnings as u32,
            errors: errors as u32,
            output_path: PathBuf::from(output_str),
            output_bytes: output_bytes as u64,
            acked: acked != 0,
        })
    })?;

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
    let now = codeapp_types::time::now_ms() as i64;
    conn.execute(
        r#"
        INSERT INTO usage (session_id, agent_id, turn, input, output, reasoning, cache_read, cache_write, cost_usd, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10);
        "#,
        params![
            session.as_str(),
            agent.as_str(),
            turn as i64,
            usage.input_tokens as i64,
            usage.output_tokens as i64,
            usage.reasoning_tokens as i64,
            usage.cache_read_tokens as i64,
            usage.cache_write_tokens as i64,
            cost_usd,
            now,
        ],
    )?;
    Ok(())
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
            COUNT(DISTINCT turn),
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
