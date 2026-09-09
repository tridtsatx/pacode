//! Schema and migrations (spec §12). `user_version` pragma tracks the version.

use rusqlite::Connection;

pub const SCHEMA_VERSION: i32 = 1;

/// Create or upgrade the schema. Idempotent.
pub fn migrate(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    let _ = conn;
    todo!("schema::migrate")
}
