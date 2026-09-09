//! Schema and migrations (spec §12). `user_version` pragma tracks the version.

use rusqlite::Connection;

pub const SCHEMA_VERSION: i32 = 1;

/// Create or upgrade the schema. Idempotent.
pub fn migrate(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    let current_version: i32 = conn.query_row("PRAGMA user_version;", [], |r| r.get(0))?;
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
                id TEXT PRIMARY KEY,
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
                error TEXT
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn migrate_twice_is_noop() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();

        let v: i32 = conn
            .query_row("PRAGMA user_version;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }
}
