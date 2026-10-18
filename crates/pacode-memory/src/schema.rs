//! Schema of one scope database (`user_version` tracks the version). Idempotent so two
//! processes migrating at once are safe.

use rusqlite::Connection;

pub const SCHEMA_VERSION: i32 = 1;

const V1: &str = r#"
BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS memory_entries (
    seq              INTEGER PRIMARY KEY AUTOINCREMENT,
    id               TEXT NOT NULL UNIQUE,
    kind             TEXT NOT NULL,
    title            TEXT NOT NULL,
    title_norm       TEXT NOT NULL,
    text             TEXT NOT NULL,
    tags             TEXT NOT NULL DEFAULT '',
    pinned           INTEGER NOT NULL DEFAULT 0,
    created_ms       INTEGER NOT NULL,
    updated_ms       INTEGER NOT NULL,
    last_recalled_ms INTEGER NOT NULL DEFAULT 0,
    recall_count     INTEGER NOT NULL DEFAULT 0,
    source           TEXT,
    superseded_by    TEXT,
    deleted_ms       INTEGER
);
CREATE INDEX IF NOT EXISTS memory_entries_title_norm ON memory_entries(title_norm);
CREATE INDEX IF NOT EXISTS memory_entries_updated ON memory_entries(updated_ms);

CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    title, text, tags,
    content='memory_entries', content_rowid='seq',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS memory_ai AFTER INSERT ON memory_entries BEGIN
    INSERT INTO memory_fts(rowid, title, text, tags) VALUES (new.seq, new.title, new.text, new.tags);
END;
CREATE TRIGGER IF NOT EXISTS memory_ad AFTER DELETE ON memory_entries BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, title, text, tags) VALUES ('delete', old.seq, old.title, old.text, old.tags);
END;
CREATE TRIGGER IF NOT EXISTS memory_au AFTER UPDATE OF title, text, tags ON memory_entries BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, title, text, tags) VALUES ('delete', old.seq, old.title, old.text, old.tags);
    INSERT INTO memory_fts(rowid, title, text, tags) VALUES (new.seq, new.title, new.text, new.tags);
END;

-- Optional semantic layer (see embed.rs); empty unless an Embedder is configured.
CREATE TABLE IF NOT EXISTS memory_embeddings (
    entry_id    TEXT NOT NULL REFERENCES memory_entries(id) ON DELETE CASCADE,
    embedder_id TEXT NOT NULL,
    dim         INTEGER NOT NULL,
    vector      BLOB NOT NULL,
    PRIMARY KEY (entry_id, embedder_id)
);
COMMIT;
"#;

pub fn migrate(conn: &Connection) -> Result<(), rusqlite::Error> {
    let version: i32 = conn.query_row("PRAGMA user_version;", [], |r| r.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    conn.execute_batch(V1)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}
