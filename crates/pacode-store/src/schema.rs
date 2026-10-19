//! Schema and migrations (spec §12). `user_version` pragma tracks the version.

use pacode_types::Message;
use rusqlite::{Connection, Transaction, params};

use crate::StoreError;

pub const SCHEMA_VERSION: i32 = 11;

const MESSAGE_TEXT_CHUNK: usize = 64;

fn add_column_if_missing(
    tx: &Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), rusqlite::Error> {
    let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    let mut exists = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            exists = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);
    if !exists {
        tx.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition};"
        ))?;
    }
    Ok(())
}

fn unsupported_schema(found: i32) -> StoreError {
    StoreError::Invalid(format!(
        "database schema v{found} is newer than supported v{SCHEMA_VERSION}"
    ))
}

fn plain_text_from_message_json(content_json: &str) -> String {
    match serde_json::from_str::<Message>(content_json) {
        Ok(message) => crate::queries::message::message_plain_text(&message),
        Err(_) => String::new(),
    }
}

/// Fills `messages.text` for rows that predate the column, so the search index can
/// be rebuilt from the table itself. Rows are taken by `id` in bounded chunks and
/// the cursor only ever moves forward, so a message whose text is legitimately
/// empty cannot keep the walk alive.
fn backfill_message_text(tx: &Transaction<'_>) -> Result<(), StoreError> {
    let mut cursor = 0i64;
    loop {
        let chunk: Vec<(i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, content_json FROM messages
                 WHERE id > ?1 AND text = ''
                 ORDER BY id ASC
                 LIMIT ?2;",
            )?;
            let rows = stmt.query_map(params![cursor, MESSAGE_TEXT_CHUNK as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut chunk = Vec::new();
            for row in rows {
                chunk.push(row?);
            }
            chunk
        };
        let Some(last_id) = chunk.last().map(|(id, _)| *id) else {
            return Ok(());
        };
        cursor = last_id;

        let mut stmt = tx.prepare("UPDATE messages SET text = ?1 WHERE id = ?2;")?;
        for (id, content_json) in &chunk {
            let text = plain_text_from_message_json(content_json);
            if text.is_empty() {
                continue;
            }
            stmt.execute(params![text, id])?;
        }
    }
}

pub fn schema_version(conn: &Connection) -> Result<i32, StoreError> {
    let version: i32 = conn.query_row("PRAGMA user_version;", [], |row| row.get(0))?;
    Ok(version)
}

/// Refuses a database written by a newer build: downgrading would let the old
/// writer corrupt tables it does not know about.
pub fn validate_schema_version(conn: &Connection) -> Result<(), StoreError> {
    let current_version = schema_version(conn)?;
    if current_version > SCHEMA_VERSION {
        return Err(unsupported_schema(current_version));
    }
    Ok(())
}

/// Create or upgrade the schema. Idempotent.
pub fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let current_version = schema_version(conn)?;
    if current_version > SCHEMA_VERSION {
        return Err(unsupported_schema(current_version));
    }
    if current_version < 1 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                name TEXT,
                cwd TEXT NOT NULL,
                git_branch TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                model TEXT NOT NULL,
                effort TEXT NOT NULL,
                mode TEXT NOT NULL,
                first_prompt TEXT,
                meta_json TEXT
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                role TEXT NOT NULL,
                content_json TEXT NOT NULL,
                hidden INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                UNIQUE(session_id, agent_id, seq)
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                text,
                content='',
                tokenize='unicode61'
            );

            CREATE TABLE IF NOT EXISTS agents (
                id TEXT NOT NULL,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                activity TEXT,
                parent_id TEXT,
                prompt TEXT,
                model TEXT NOT NULL,
                effort TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                tokens_in INTEGER NOT NULL DEFAULT 0,
                tokens_out INTEGER NOT NULL DEFAULT 0,
                summary TEXT,
                error TEXT,
                PRIMARY KEY (session_id, id)
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                owner_agent TEXT NOT NULL,
                label TEXT NOT NULL,
                command TEXT NOT NULL,
                cwd TEXT NOT NULL,
                status TEXT NOT NULL,
                backgrounded INTEGER NOT NULL DEFAULT 0,
                exit_code INTEGER,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                output_path TEXT NOT NULL,
                output_bytes INTEGER NOT NULL DEFAULT 0,
                progress_json TEXT,
                warnings INTEGER NOT NULL DEFAULT 0,
                errors INTEGER NOT NULL DEFAULT 0,
                acked INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS plans (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                version INTEGER NOT NULL DEFAULT 0,
                json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id TEXT NOT NULL,
                turn INTEGER NOT NULL,
                input INTEGER NOT NULL,
                output INTEGER NOT NULL,
                reasoning INTEGER NOT NULL DEFAULT 0,
                cache_read INTEGER NOT NULL DEFAULT 0,
                cache_write INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS compaction (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                upto_seq INTEGER NOT NULL,
                PRIMARY KEY(session_id, agent_id)
            );

            CREATE INDEX IF NOT EXISTS idx_messages_lookup ON messages(session_id, agent_id, seq);
            CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions(updated_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_cwd ON sessions(cwd);
            CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(session_id);
            CREATE INDEX IF NOT EXISTS idx_agents_session ON agents(session_id);
            CREATE INDEX IF NOT EXISTS idx_usage_session ON usage(session_id);

            PRAGMA user_version = 1;
            "#,
        )?;
        tx.commit()?;
    }

    if current_version < 2 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS cron_jobs (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                schedule TEXT NOT NULL,
                prompt TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                last_run_at INTEGER,
                next_run_at INTEGER,
                last_status TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_cron_jobs_session ON cron_jobs(session_id);

            PRAGMA user_version = 2;
            "#,
        )?;
        tx.commit()?;
    }

    // v3 and v4 also run for v3/v4 databases: a pre-merge branch build used
    // those two numbers for other steps (compaction audit columns, now v11; an
    // FTS rebuild, superseded by v9). Both steps are idempotent.
    if current_version < 5 {
        let tx = conn.transaction()?;
        add_column_if_missing(&tx, "sessions", "zdr", "INTEGER NOT NULL DEFAULT 0")?;
        tx.execute_batch("PRAGMA user_version = 3;")?;
        tx.commit()?;
    }
    if current_version < 5 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS message_admissions (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                message_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, message_id)
            );

            PRAGMA user_version = 4;
            "#,
        )?;
        tx.commit()?;
    }
    if current_version < 5 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS attach_admissions (
                client_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                session_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                allow_create INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                PRIMARY KEY(client_id, idempotency_key)
            );

            CREATE INDEX IF NOT EXISTS idx_attach_admissions_session
                ON attach_admissions(session_id);

            PRAGMA user_version = 5;
            "#,
        )?;
        tx.commit()?;
    }
    if current_version < 6 {
        let tx = conn.transaction()?;
        add_column_if_missing(
            &tx,
            "message_admissions",
            "agent_id",
            "TEXT NOT NULL DEFAULT 'main'",
        )?;
        add_column_if_missing(&tx, "message_admissions", "message_seq", "INTEGER")?;
        add_column_if_missing(
            &tx,
            "message_admissions",
            "state",
            "TEXT NOT NULL DEFAULT 'admitted'",
        )?;
        add_column_if_missing(&tx, "message_admissions", "turn_id", "TEXT")?;
        add_column_if_missing(&tx, "message_admissions", "terminal_stop", "TEXT")?;
        add_column_if_missing(&tx, "usage", "turn_id", "TEXT")?;
        tx.execute_batch(
            r#"
            CREATE INDEX IF NOT EXISTS idx_message_admissions_session_state
                ON message_admissions(session_id, state, message_seq);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_turn_id
                ON usage(session_id, turn_id)
                WHERE turn_id IS NOT NULL;

            PRAGMA user_version = 6;
            "#,
        )?;
        tx.commit()?;
    }
    if current_version < 7 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE agents_v7 (
                id TEXT NOT NULL,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                activity TEXT,
                parent_id TEXT,
                prompt TEXT,
                model TEXT NOT NULL,
                effort TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                tokens_in INTEGER NOT NULL DEFAULT 0,
                tokens_out INTEGER NOT NULL DEFAULT 0,
                summary TEXT,
                error TEXT,
                PRIMARY KEY (session_id, id)
            );
            INSERT INTO agents_v7 (
                id, session_id, name, kind, status, activity, parent_id, prompt,
                model, effort, started_at, finished_at, tokens_in, tokens_out, summary, error
            )
            SELECT id, session_id, name, kind, status, activity, parent_id, prompt,
                model, effort, started_at, finished_at, tokens_in, tokens_out, summary, error
            FROM agents;
            DROP TABLE agents;
            ALTER TABLE agents_v7 RENAME TO agents;
            CREATE INDEX IF NOT EXISTS idx_agents_session ON agents(session_id);
            PRAGMA user_version = 7;
            "#,
        )?;
        tx.commit()?;
    }
    if current_version < 8 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS task_admissions (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                idempotency_key TEXT NOT NULL,
                task_id TEXT NOT NULL,
                owner_agent TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, idempotency_key)
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_task_admissions_task
                ON task_admissions(task_id);
            CREATE INDEX IF NOT EXISTS idx_task_admissions_created
                ON task_admissions(session_id, created_at);

            CREATE TEMP TABLE usage_legacy_group AS
            SELECT
                MIN(id) AS keeper_id,
                session_id,
                agent_id,
                turn,
                SUM(input) AS input,
                SUM(output) AS output,
                SUM(reasoning) AS reasoning,
                SUM(cache_read) AS cache_read,
                SUM(cache_write) AS cache_write,
                SUM(cost_usd) AS cost_usd,
                COUNT(cost_usd) AS cost_rows
            FROM usage
            WHERE turn_id IS NULL
            GROUP BY session_id, agent_id, turn
            HAVING COUNT(*) > 1;

            CREATE TEMP TABLE usage_legacy_dup AS
            SELECT usage.id AS id
            FROM usage
            JOIN usage_legacy_group AS group_totals
              ON group_totals.session_id = usage.session_id
             AND group_totals.agent_id = usage.agent_id
             AND group_totals.turn = usage.turn
            WHERE usage.turn_id IS NULL
              AND usage.id <> group_totals.keeper_id;

            UPDATE usage SET
                input = (SELECT group_totals.input
                         FROM usage_legacy_group AS group_totals
                         WHERE group_totals.keeper_id = usage.id),
                output = (SELECT group_totals.output
                          FROM usage_legacy_group AS group_totals
                          WHERE group_totals.keeper_id = usage.id),
                reasoning = (SELECT group_totals.reasoning
                             FROM usage_legacy_group AS group_totals
                             WHERE group_totals.keeper_id = usage.id),
                cache_read = (SELECT group_totals.cache_read
                              FROM usage_legacy_group AS group_totals
                              WHERE group_totals.keeper_id = usage.id),
                cache_write = (SELECT group_totals.cache_write
                               FROM usage_legacy_group AS group_totals
                               WHERE group_totals.keeper_id = usage.id),
                cost_usd = (SELECT CASE WHEN group_totals.cost_rows = 0 THEN NULL
                                       ELSE group_totals.cost_usd END
                            FROM usage_legacy_group AS group_totals
                            WHERE group_totals.keeper_id = usage.id)
            WHERE id IN (SELECT keeper_id FROM usage_legacy_group);

            DELETE FROM usage
            WHERE id IN (SELECT id FROM usage_legacy_dup);

            DROP TABLE usage_legacy_group;
            DROP TABLE usage_legacy_dup;

            CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_legacy_turn
                ON usage(session_id, agent_id, turn)
                WHERE turn_id IS NULL;

            DROP INDEX IF EXISTS idx_message_admissions_session_agent_state;
            CREATE INDEX idx_message_admissions_session_agent_state
                ON message_admissions(session_id, agent_id, state, message_seq);
            DROP INDEX IF EXISTS idx_message_admissions_page;
            CREATE INDEX idx_message_admissions_page
                ON message_admissions(session_id, message_seq, message_id);
            DROP INDEX IF EXISTS idx_message_admissions_unresolved;
            CREATE INDEX idx_message_admissions_unresolved
                ON message_admissions(session_id, agent_id, message_seq, message_id)
                WHERE state <> 'terminal';

            DROP INDEX IF EXISTS idx_sessions_updated_at;
            DROP INDEX IF EXISTS idx_sessions_cwd;
            DROP INDEX IF EXISTS idx_sessions_updated_id;
            DROP INDEX IF EXISTS idx_sessions_cwd_updated;
            CREATE INDEX idx_sessions_updated_id
                ON sessions(updated_at DESC, id ASC);
            CREATE INDEX idx_sessions_cwd_updated
                ON sessions(cwd, updated_at DESC, id ASC);

            DROP INDEX IF EXISTS idx_agents_session;
            DROP INDEX IF EXISTS idx_agents_session_started;
            CREATE INDEX idx_agents_session_started
                ON agents(session_id, started_at, id);

            DROP INDEX IF EXISTS idx_tasks_session;
            DROP INDEX IF EXISTS idx_tasks_session_started;
            CREATE INDEX idx_tasks_session_started
                ON tasks(session_id, started_at, id);

            PRAGMA user_version = 8;
            "#,
        )?;
        tx.commit()?;
    }
    if current_version < 9 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            r#"
            DROP TRIGGER IF EXISTS messages_fts_ai;
            DROP TRIGGER IF EXISTS messages_fts_au;
            DROP TRIGGER IF EXISTS messages_fts_ad;
            DROP TABLE IF EXISTS messages_fts;
            "#,
        )?;
        add_column_if_missing(&tx, "messages", "text", "TEXT NOT NULL DEFAULT ''")?;
        backfill_message_text(&tx)?;
        tx.execute_batch(
            r#"
            CREATE VIRTUAL TABLE messages_fts USING fts5(
                text,
                content='messages',
                content_rowid='id',
                tokenize='unicode61'
            );

            INSERT INTO messages_fts (rowid, text)
            SELECT id, text FROM messages WHERE text <> '';

            CREATE TRIGGER messages_fts_ai AFTER INSERT ON messages
            WHEN new.text <> '' BEGIN
                INSERT INTO messages_fts (rowid, text) VALUES (new.id, new.text);
            END;

            CREATE TRIGGER messages_fts_au AFTER UPDATE ON messages
            WHEN old.text <> new.text BEGIN
                INSERT INTO messages_fts (messages_fts, rowid, text)
                VALUES ('delete', old.id, old.text);
                INSERT INTO messages_fts (rowid, text) VALUES (new.id, new.text);
            END;

            CREATE TRIGGER messages_fts_ad AFTER DELETE ON messages
            WHEN old.text <> '' BEGIN
                INSERT INTO messages_fts (messages_fts, rowid, text)
                VALUES ('delete', old.id, old.text);
            END;

            PRAGMA user_version = 9;
            "#,
        )?;
        tx.commit()?;
    }
    if current_version < 10 {
        // What a reopened subagent must come back with that `AgentInfo` does
        // not carry: who stopped it (`'user'` / `'orchestrator'`) and the tool
        // names it was spawned with (a JSON array). NULL — every row written
        // before — means no stop on record and the default subagent set.
        let tx = conn.transaction()?;
        add_column_if_missing(&tx, "agents", "stopped_by", "TEXT")?;
        add_column_if_missing(&tx, "agents", "tools_json", "TEXT")?;
        tx.execute_batch("PRAGMA user_version = 10;")?;
        tx.commit()?;
    }
    if current_version < 11 {
        // Compaction audit columns: which stage produced the row and token counts.
        let tx = conn.transaction()?;
        add_column_if_missing(
            &tx,
            "compaction",
            "method",
            "TEXT NOT NULL DEFAULT 'summary'",
        )?;
        add_column_if_missing(
            &tx,
            "compaction",
            "tokens_before",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        add_column_if_missing(
            &tx,
            "compaction",
            "tokens_after",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        tx.execute_batch("PRAGMA user_version = 11;")?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index';")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1;",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .is_ok()
    }

    fn trigger_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1;",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .is_ok()
    }

    fn table_sql(conn: &Connection, name: &str) -> String {
        conn.query_row(
            "SELECT COALESCE(sql, '') FROM sqlite_master WHERE type = 'table' AND name = ?1;",
            [name],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    }

    fn index_sql(conn: &Connection, name: &str) -> String {
        conn.query_row(
            "SELECT COALESCE(sql, '') FROM sqlite_master WHERE type = 'index' AND name = ?1;",
            [name],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    }

    fn legacy_usage_ledger(
        conn: &Connection,
        session: &str,
    ) -> Vec<(String, i64, i64, Option<f64>)> {
        let mut stmt = conn
            .prepare(
                "SELECT agent_id, turn, input, cost_usd FROM usage WHERE session_id = ?1 ORDER BY agent_id, turn;",
            )
            .unwrap();
        stmt.query_map([session], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<f64>>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    }

    fn insert_legacy_usage(
        conn: &Connection,
        session: &str,
        agent: &str,
        turn: i64,
        input: i64,
        cost: f64,
    ) {
        conn.execute(
            "INSERT OR IGNORE INTO sessions (id, cwd, created_at, updated_at, model, effort, mode) VALUES (?1, '.', 0, 0, 'mock/mock', 'low', 'build');",
            [session],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage (session_id, agent_id, turn, input, output, reasoning, cache_read, cache_write, cost_usd, created_at) VALUES (?1, ?2, ?3, ?4, 0, 0, 0, 0, ?5, 0);",
            rusqlite::params![session, agent, turn, input, cost],
        )
        .unwrap();
    }

    #[test]
    fn migrate_creates_tables_and_sets_user_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        let v: i32 = conn
            .query_row("PRAGMA user_version;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn migrate_v6_rebuilds_session_scoped_agent_identity() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch("PRAGMA user_version = 6;").unwrap();
        migrate(&mut conn).unwrap();
        let columns = conn
            .prepare("PRAGMA table_info(agents);")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            columns
                .iter()
                .find(|(name, _)| name == "session_id")
                .map(|(_, pk)| *pk),
            Some(1)
        );
        assert_eq!(
            columns
                .iter()
                .find(|(name, _)| name == "id")
                .map(|(_, pk)| *pk),
            Some(2)
        );
    }

    #[test]
    fn migrate_twice_is_noop() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();

        let v: i32 = conn
            .query_row("PRAGMA user_version;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn migrate_v4_adds_attach_admissions() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch("DROP TABLE attach_admissions; PRAGMA user_version = 4;")
            .unwrap();

        migrate(&mut conn).unwrap();

        let table: String = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'attach_admissions';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, "attach_admissions");
        let version: i32 = conn
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn migrate_v3_adds_message_admissions() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch("DROP TABLE message_admissions; PRAGMA user_version = 3;")
            .unwrap();

        migrate(&mut conn).unwrap();

        let table: String = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'message_admissions';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, "message_admissions");
        let version: i32 = conn
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn migrate_refuses_newer_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
            .unwrap();

        let error = migrate(&mut conn).unwrap_err();
        assert!(
            matches!(&error, StoreError::Invalid(message) if message.contains("newer")),
            "unexpected error: {error}"
        );
        let error = validate_schema_version(&conn).unwrap_err();
        assert!(
            matches!(&error, StoreError::Invalid(message) if message.contains("newer")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn validate_schema_version_accepts_supported_and_missing_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        validate_schema_version(&conn).unwrap();

        migrate(&mut conn).unwrap();
        validate_schema_version(&conn).unwrap();
    }

    fn agent_columns(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("PRAGMA table_info(agents)").unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    /// A v9 agents table gains the stop origin and the spawn-time tool set as
    /// NULL — no stop on record, the default tools — and keeps its rows.
    #[test]
    fn migrate_v10_adds_the_subagent_restore_columns() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch(
            r#"
            ALTER TABLE agents DROP COLUMN stopped_by;
            ALTER TABLE agents DROP COLUMN tools_json;
            INSERT INTO sessions (id, cwd, created_at, updated_at, model, effort, mode)
            VALUES ('ses_v9', '.', 0, 0, 'mock/mock', 'low', 'build');
            INSERT INTO agents (id, session_id, name, kind, status, model, effort, started_at)
            VALUES ('agt_sub', 'ses_v9', 'sub', 'sub', 'stopped', 'mock/mock', 'low', 0);
            PRAGMA user_version = 9;
            "#,
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();

        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        let columns = agent_columns(&conn);
        assert!(columns.iter().any(|c| c == "stopped_by"), "{columns:?}");
        assert!(columns.iter().any(|c| c == "tools_json"), "{columns:?}");
        let (stopped_by, tools): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT stopped_by, tools_json FROM agents WHERE id = 'agt_sub';",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((stopped_by, tools), (None, None));
    }

    #[test]
    fn migrate_from_every_version_boundary_is_idempotent() {
        for start in 0..=SCHEMA_VERSION {
            let mut conn = Connection::open_in_memory().unwrap();
            migrate(&mut conn).unwrap();
            conn.execute_batch(&format!("PRAGMA user_version = {start};"))
                .unwrap();

            migrate(&mut conn).unwrap();
            migrate(&mut conn).unwrap();

            assert_eq!(
                schema_version(&conn).unwrap(),
                SCHEMA_VERSION,
                "boundary {start} did not converge"
            );
            assert!(table_exists(&conn, "task_admissions"), "boundary {start}");
            let columns = agent_columns(&conn);
            for expected in ["stopped_by", "tools_json"] {
                assert!(
                    columns.iter().any(|c| c == expected),
                    "boundary {start} is missing agents.{expected}"
                );
            }
            let indices = index_names(&conn);
            for expected in [
                "idx_usage_legacy_turn",
                "idx_message_admissions_session_agent_state",
                "idx_message_admissions_page",
                "idx_message_admissions_unresolved",
                "idx_sessions_updated_id",
                "idx_sessions_cwd_updated",
                "idx_agents_session_started",
                "idx_tasks_session_started",
                "idx_task_admissions_task",
                "idx_task_admissions_created",
            ] {
                assert!(
                    indices.iter().any(|name| name == expected),
                    "boundary {start} is missing {expected}"
                );
            }
            assert!(
                !indices.iter().any(|name| name == "idx_sessions_updated_at"),
                "boundary {start} kept the superseded index"
            );
            assert!(
                !indices.iter().any(|name| name == "idx_agents_session"),
                "boundary {start} kept the superseded agent index"
            );
            let index_sql = table_sql(&conn, "messages_fts");
            assert!(
                index_sql.contains("content='messages'")
                    && index_sql.contains("content_rowid='id'"),
                "boundary {start} kept a message index that is not backed by messages: {index_sql}"
            );
            for trigger in ["messages_fts_ai", "messages_fts_au", "messages_fts_ad"] {
                assert!(
                    trigger_exists(&conn, trigger),
                    "boundary {start} is missing {trigger}"
                );
            }
        }
    }

    /// Rewinds a migrated database to the v8 shape: a contentless index, no `text`
    /// column, and a message that only the contentless index knows about.
    fn rewind_to_v8_messages(conn: &Connection, text: &str) -> i64 {
        conn.execute_batch(
            r#"
            DROP TRIGGER messages_fts_ai;
            DROP TRIGGER messages_fts_au;
            DROP TRIGGER messages_fts_ad;
            DROP TABLE messages_fts;
            ALTER TABLE messages DROP COLUMN text;
            CREATE VIRTUAL TABLE messages_fts USING fts5(
                text,
                content='',
                tokenize='unicode61'
            );
            PRAGMA user_version = 8;
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, cwd, created_at, updated_at, model, effort, mode) VALUES ('ses_v8', '.', 0, 0, 'mock/mock', 'low', 'build');",
            [],
        )
        .unwrap();
        let json = serde_json::to_string(&Message::user(text)).unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, agent_id, seq, role, content_json, hidden, created_at) VALUES ('ses_v8', 'agt_main', 0, 'user', ?1, 0, 0);",
            [&json],
        )
        .unwrap();
        let rowid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO messages_fts (rowid, text) VALUES (?1, ?2);",
            rusqlite::params![rowid, text],
        )
        .unwrap();
        rowid
    }

    #[test]
    fn migrate_v9_moves_the_message_index_onto_the_messages_table() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let rowid = rewind_to_v8_messages(&conn, "Разработка persistence на Rust");

        migrate(&mut conn).unwrap();

        let index_sql = table_sql(&conn, "messages_fts");
        assert!(index_sql.contains("content='messages'"), "{index_sql}");
        assert!(index_sql.contains("content_rowid='id'"), "{index_sql}");
        let text: String = conn
            .query_row("SELECT text FROM messages WHERE id = ?1;", [rowid], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(text, "Разработка persistence на Rust");
        let hits = crate::queries::search(&conn, "Разработка", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 0);
        assert!(hits[0].snippet.contains("[Разработка]"));
    }

    #[test]
    fn migrate_v9_backfills_only_the_messages_it_has_to() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        rewind_to_v8_messages(&conn, "первое сообщение");
        conn.execute(
            "INSERT INTO messages (session_id, agent_id, seq, role, content_json, hidden, created_at) VALUES ('ses_v8', 'agt_main', 1, 'user', '{\"role\":\"user\"}', 0, 0);",
            [],
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();

        let text: String = conn
            .query_row(
                "SELECT text FROM messages WHERE session_id = 'ses_v8' AND seq = 1;",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(text, "", "a message that is not a message stays empty");
        let indexed: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages_fts_docsize;", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(indexed, 1, "only the readable message belongs in the index");
    }

    #[test]
    fn migrate_v8_merges_duplicate_legacy_usage_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch("DROP INDEX IF EXISTS idx_usage_legacy_turn; PRAGMA user_version = 7;")
            .unwrap();
        insert_legacy_usage(&conn, "ses_legacy", "agt_main", 1, 10, 0.01);
        insert_legacy_usage(&conn, "ses_legacy", "agt_main", 1, 20, 0.02);
        insert_legacy_usage(&conn, "ses_legacy", "agt_main", 1, 30, 0.03);
        insert_legacy_usage(&conn, "ses_legacy", "agt_sub", 1, 5, 0.5);
        insert_legacy_usage(&conn, "ses_legacy", "agt_main", 2, 7, 0.7);
        let before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM usage WHERE session_id = 'ses_legacy';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, 5);

        migrate(&mut conn).unwrap();

        let rows = conn
            .prepare(
                "SELECT agent_id, turn, input, cost_usd FROM usage WHERE session_id = 'ses_legacy' ORDER BY agent_id, turn;",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "agt_main");
        assert_eq!(rows[0].1, 1);
        assert_eq!(rows[0].2, 60);
        assert!((rows[0].3.unwrap() - 0.06).abs() < 1e-9);
        assert_eq!(
            (rows[1].0.as_str(), rows[1].1, rows[1].2),
            ("agt_main", 2, 7)
        );
        assert!((rows[1].3.unwrap() - 0.7).abs() < 1e-9);
        assert_eq!(
            (rows[2].0.as_str(), rows[2].1, rows[2].2),
            ("agt_sub", 1, 5)
        );
        assert!((rows[2].3.unwrap() - 0.5).abs() < 1e-9);

        conn.execute(
            "INSERT INTO usage (session_id, agent_id, turn, input, output, reasoning, cache_read, cache_write, created_at) VALUES ('ses_legacy', 'agt_main', 1, 1, 0, 0, 0, 0, 0);",
            [],
        )
        .unwrap_err();
        let after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM usage WHERE session_id = 'ses_legacy';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, 3);
    }

    #[test]
    fn migrate_v8_keeps_missing_legacy_cost_null() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch("DROP INDEX IF EXISTS idx_usage_legacy_turn; PRAGMA user_version = 7;")
            .unwrap();
        insert_legacy_usage(&conn, "ses_cost", "agt_main", 1, 10, 0.0);
        conn.execute(
            "UPDATE usage SET cost_usd = NULL WHERE session_id = 'ses_cost';",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage (session_id, agent_id, turn, input, output, reasoning, cache_read, cache_write, cost_usd, created_at) VALUES ('ses_cost', 'agt_main', 1, 5, 0, 0, 0, 0, NULL, 0);",
            [],
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let row = conn
            .prepare("SELECT input, cost_usd FROM usage WHERE session_id = 'ses_cost';")
            .unwrap()
            .query_row([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<f64>>(1)?))
            })
            .unwrap();
        assert_eq!(row, (15, None));
    }

    fn seed_legacy_duplicates(conn: &Connection, session: &str) {
        insert_legacy_usage(conn, session, "agt_main", 1, 10, 0.01);
        insert_legacy_usage(conn, session, "agt_main", 1, 20, 0.02);
        insert_legacy_usage(conn, session, "agt_main", 1, 30, 0.03);
        insert_legacy_usage(conn, session, "agt_main", 1, 40, 0.04);
        insert_legacy_usage(conn, session, "agt_main", 2, 5, 0.05);
        insert_legacy_usage(conn, session, "agt_main", 2, 6, 0.06);
        insert_legacy_usage(conn, session, "agt_sub", 7, 7, 0.07);
    }

    #[test]
    fn migrate_v8_merges_legacy_usage_the_same_whatever_order_rows_are_read() {
        fn merge(session: &str, pragma: &str) -> Vec<(String, i64, i64, Option<f64>)> {
            let mut conn = Connection::open_in_memory().unwrap();
            migrate(&mut conn).unwrap();
            conn.execute_batch(
                "DROP INDEX IF EXISTS idx_usage_legacy_turn; PRAGMA user_version = 7;",
            )
            .unwrap();
            seed_legacy_duplicates(&conn, session);
            conn.execute_batch(pragma).unwrap();

            migrate(&mut conn).unwrap();

            legacy_usage_ledger(&conn, session)
        }

        let forward = merge(
            "ses_order_forward",
            "PRAGMA reverse_unordered_selects = OFF;",
        );
        let reversed = merge(
            "ses_order_reversed",
            "PRAGMA reverse_unordered_selects = ON;",
        );

        assert_eq!(forward, reversed);
        assert_eq!(forward.len(), 3);
        assert_eq!(
            (forward[0].0.as_str(), forward[0].1, forward[0].2),
            ("agt_main", 1, 100)
        );
        assert!((forward[0].3.unwrap() - 0.1).abs() < 1e-9);
        assert_eq!(
            (forward[1].0.as_str(), forward[1].1, forward[1].2),
            ("agt_main", 2, 11)
        );
        assert!((forward[1].3.unwrap() - 0.11).abs() < 1e-9);
        assert_eq!(
            (forward[2].0.as_str(), forward[2].1, forward[2].2),
            ("agt_sub", 7, 7)
        );
        assert!((forward[2].3.unwrap() - 0.07).abs() < 1e-9);
    }

    #[test]
    fn migrate_v8_leaves_single_row_legacy_groups_untouched() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch("DROP INDEX IF EXISTS idx_usage_legacy_turn; PRAGMA user_version = 7;")
            .unwrap();
        insert_legacy_usage(&conn, "ses_single", "agt_main", 1, 42, 0.42);
        let keeper: i64 = conn
            .query_row(
                "SELECT id FROM usage WHERE session_id = 'ses_single';",
                [],
                |row| row.get(0),
            )
            .unwrap();

        migrate(&mut conn).unwrap();

        let (id, input) = conn
            .prepare("SELECT id, input FROM usage WHERE session_id = 'ses_single';")
            .unwrap()
            .query_row([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .unwrap();
        assert_eq!((id, input), (keeper, 42));
    }

    #[test]
    fn migrate_v8_keeps_turn_id_ledgers_out_of_the_legacy_merge() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute_batch("DROP INDEX IF EXISTS idx_usage_legacy_turn; PRAGMA user_version = 7;")
            .unwrap();
        insert_legacy_usage(&conn, "ses_mixed", "agt_main", 1, 10, 0.1);
        insert_legacy_usage(&conn, "ses_mixed", "agt_main", 1, 20, 0.2);
        conn.execute(
            "INSERT INTO usage (session_id, agent_id, turn, turn_id, input, output, reasoning, cache_read, cache_write, cost_usd, created_at) VALUES ('ses_mixed', 'agt_main', 1, 'trn-1', 3, 0, 0, 0, 0, 0.3, 0);",
            [],
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT turn_id, input, cost_usd FROM usage WHERE session_id = 'ses_mixed' ORDER BY turn_id IS NOT NULL, id;",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, None);
        assert_eq!(rows[0].1, 30);
        assert!((rows[0].2.unwrap() - 0.3).abs() < 1e-9);
        assert_eq!(rows[1].0.as_deref(), Some("trn-1"));
        assert_eq!(rows[1].1, 3);
    }

    #[test]
    fn keyset_indexes_match_the_order_the_paging_queries_ask_for() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        let sessions = index_sql(&conn, "idx_sessions_updated_id");
        assert!(
            sessions.contains("updated_at DESC"),
            "session paging orders by updated_at DESC: {sessions}"
        );
        assert!(
            sessions.contains("id ASC"),
            "session paging breaks ties by id: {sessions}"
        );

        let scoped_sessions = index_sql(&conn, "idx_sessions_cwd_updated");
        assert!(
            scoped_sessions.contains("updated_at DESC"),
            "{scoped_sessions}"
        );
        assert!(scoped_sessions.contains("id ASC"), "{scoped_sessions}");

        let admissions = index_sql(&conn, "idx_message_admissions_page");
        assert!(
            admissions.contains("session_id, message_seq, message_id"),
            "{admissions}"
        );

        let unresolved = index_sql(&conn, "idx_message_admissions_unresolved");
        assert!(
            unresolved.contains("message_seq, message_id"),
            "{unresolved}"
        );
        assert!(unresolved.contains("state <> 'terminal'"), "{unresolved}");
    }
}
