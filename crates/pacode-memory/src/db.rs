//! One scope database: open (WAL, busy timeout), migrate, purge old tombstones,
//! import a legacy `memory.md`. Owned by [`crate::MemoryStore`] behind a `Mutex`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::MemoryError;
use crate::{legacy, ops, schema};

/// Forgotten / superseded entries older than this are hard-deleted at open.
pub const PURGE_AFTER_MS: i64 = 90 * 24 * 60 * 60 * 1000;
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

pub(crate) struct ScopeDb {
    pub(crate) conn: Connection,
}

/// Path of the legacy flat file next to a database (`memory.db` -> `memory.md`).
pub(crate) fn legacy_path(db_path: &Path) -> PathBuf {
    db_path.with_file_name("memory.md")
}

impl ScopeDb {
    pub(crate) fn open(path: &Path, now_ms: i64) -> Result<Self, MemoryError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // journal_mode returns a row, so it cannot go through `execute`.
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL;", [], |r| r.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&conn)?;
        let mut db = Self { conn };
        if let Err(e) = ops::purge_before(&db.conn, now_ms - PURGE_AFTER_MS) {
            log::warn!("memory purge failed for {}: {e}", path.display());
        }
        let md = legacy_path(path);
        if md.is_file()
            && let Err(e) = legacy::import(&mut db.conn, &md, now_ms)
        {
            log::warn!("memory migration of {} failed: {e}", md.display());
        }
        Ok(db)
    }
}
