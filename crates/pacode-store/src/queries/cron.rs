//! Cron job persistence. Jobs outlive the daemon; monitors never reach the store.

use pacode_types::{CronJob, CronJobId, CronSchedule, SessionId};
use rusqlite::{Connection, params};

use crate::StoreError;

#[cfg(test)]
#[path = "cron_tests.rs"]
mod cron_tests;

/// Longest values accepted into the table. A prompt is what a fired job sends to
/// the model, so it is capped like everything else that reaches the context.
pub const NAME_MAX_CHARS: usize = 120;
pub const PROMPT_MAX_CHARS: usize = 8000;

fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

pub fn upsert_cron_job(
    conn: &Connection,
    session: &SessionId,
    job: &CronJob,
) -> Result<(), StoreError> {
    let schedule = serde_json::to_string(&job.schedule)?;
    conn.execute(
        r#"
        INSERT INTO cron_jobs
            (id, session_id, name, schedule, prompt, enabled, created_at, last_run_at, next_run_at, last_status)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            schedule = excluded.schedule,
            prompt = excluded.prompt,
            enabled = excluded.enabled,
            last_run_at = excluded.last_run_at,
            next_run_at = excluded.next_run_at,
            last_status = excluded.last_status
        "#,
        params![
            job.id.to_string(),
            session.to_string(),
            cap(&job.name, NAME_MAX_CHARS),
            schedule,
            cap(&job.prompt, PROMPT_MAX_CHARS),
            job.enabled as i64,
            job.created_at_ms as i64,
            job.last_run_ms.map(|v| v as i64),
            job.next_run_ms.map(|v| v as i64),
            job.last_status.as_deref(),
        ],
    )?;
    Ok(())
}

pub fn delete_cron_job(conn: &Connection, id: &CronJobId) -> Result<bool, StoreError> {
    let removed = conn.execute(
        "DELETE FROM cron_jobs WHERE id = ?1",
        params![id.to_string()],
    )?;
    Ok(removed > 0)
}

pub fn list_cron_jobs(conn: &Connection, session: &SessionId) -> Result<Vec<CronJob>, StoreError> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, name, schedule, prompt, enabled, created_at, last_run_at, next_run_at, last_status
        FROM cron_jobs
        WHERE session_id = ?1
        ORDER BY created_at
        "#,
    )?;
    let rows = stmt.query_map(params![session.to_string()], |row| {
        let schedule_json: String = row.get(2)?;
        Ok((
            CronJobId::new(row.get::<_, String>(0)?),
            row.get::<_, String>(1)?,
            schedule_json,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)? != 0,
            row.get::<_, i64>(5)? as u64,
            row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
            row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
            row.get::<_, Option<String>>(8)?,
        ))
    })?;

    let mut jobs = Vec::new();
    for row in rows {
        let (id, name, schedule_json, prompt, enabled, created, last_run, next_run, last_status) =
            row?;
        // A row whose schedule no longer parses is skipped rather than failing the
        // whole listing: one bad row must not hide every other job.
        let schedule: CronSchedule = match serde_json::from_str(&schedule_json) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("cron job {id} has an unreadable schedule, skipping it: {e}");
                continue;
            }
        };
        jobs.push(CronJob {
            id,
            name,
            schedule,
            prompt,
            enabled,
            created_at_ms: created,
            last_run_ms: last_run,
            next_run_ms: next_run,
            last_status,
        });
    }
    Ok(jobs)
}
