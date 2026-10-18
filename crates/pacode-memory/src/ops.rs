//! SQL operations on one scope database. Free functions over `&Connection` so they run
//! equally inside a transaction (`Transaction` derefs to `Connection`).

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::MemoryError;
use crate::text::{fts_query, jaccard, normalize_title, token_set};
use crate::types::{Entry, Kind, NewEntry, Scope};

pub(crate) const COLS: &str = "e.id, e.kind, e.title, e.text, e.tags, e.pinned, e.created_ms, \
    e.updated_ms, e.last_recalled_ms, e.recall_count, e.source, e.superseded_by";
/// Entries that are neither forgotten nor superseded.
pub(crate) const LIVE: &str = "e.deleted_ms IS NULL AND e.superseded_by IS NULL";
/// Jaccard threshold above which a write updates the existing entry.
pub(crate) const DEDUPE_JACCARD: f64 = 0.8;
const DEDUPE_CANDIDATES: usize = 25;

pub(crate) fn row_entry(scope: Scope, row: &Row<'_>) -> rusqlite::Result<Entry> {
    let kind: String = row.get(1)?;
    let tags: String = row.get(4)?;
    Ok(Entry {
        id: row.get(0)?,
        scope,
        kind: Kind::parse(&kind).unwrap_or(Kind::Fact),
        title: row.get(2)?,
        text: row.get(3)?,
        tags: tags
            .split(',')
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect(),
        pinned: row.get::<_, i64>(5)? != 0,
        created_ms: row.get(6)?,
        updated_ms: row.get(7)?,
        last_recalled_ms: row.get(8)?,
        recall_count: row.get(9)?,
        source: row.get(10)?,
        superseded_by: row.get(11)?,
    })
}

pub(crate) fn insert(
    conn: &Connection,
    id: &str,
    n: &NewEntry,
    now: i64,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO memory_entries (id, kind, title, title_norm, text, tags, pinned, \
         created_ms, updated_ms, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)",
        params![
            id,
            n.kind.as_str(),
            n.title,
            normalize_title(&n.title),
            n.text,
            n.tags.join(","),
            n.pinned as i64,
            now,
            n.source,
        ],
    )?;
    Ok(())
}

/// Overwrite an existing entry in place. `pinned` only ever goes up here.
pub(crate) fn update(
    conn: &Connection,
    id: &str,
    n: &NewEntry,
    now: i64,
) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memory_entries SET kind = ?2, title = ?3, title_norm = ?4, text = ?5, \
         tags = ?6, pinned = MAX(pinned, ?7), updated_ms = ?8, source = COALESCE(?9, source) \
         WHERE id = ?1",
        params![
            id,
            n.kind.as_str(),
            n.title,
            normalize_title(&n.title),
            n.text,
            n.tags.join(","),
            n.pinned as i64,
            now,
            n.source,
        ],
    )?;
    Ok(())
}

/// Hide `old` behind `new`. Returns whether a live entry was superseded.
pub(crate) fn supersede(
    conn: &Connection,
    old: &str,
    new: &str,
    now: i64,
) -> Result<bool, MemoryError> {
    let n = conn.execute(
        "UPDATE memory_entries SET superseded_by = ?2, updated_ms = ?3 \
         WHERE id = ?1 AND id <> ?2 AND deleted_ms IS NULL AND superseded_by IS NULL",
        params![old, new, now],
    )?;
    Ok(n > 0)
}

/// Id of a live entry that is a near-duplicate of `n`: same normalised title, or Jaccard
/// of the title+text token sets >= 0.8 among the top FTS candidates.
pub(crate) fn find_duplicate(
    conn: &Connection,
    scope: Scope,
    n: &NewEntry,
) -> Result<Option<String>, MemoryError> {
    let sql = format!(
        "SELECT e.id FROM memory_entries e WHERE e.title_norm = ?1 AND {LIVE} \
         ORDER BY e.updated_ms DESC LIMIT 1"
    );
    let exact: Option<String> = conn
        .query_row(&sql, [normalize_title(&n.title)], |r| r.get(0))
        .optional()?;
    if exact.is_some() {
        return Ok(exact);
    }
    let blob = format!("{} {}", n.title, n.text);
    let Some(q) = fts_query(&blob) else {
        return Ok(None);
    };
    let want = token_set(&blob);
    for (e, _) in search(conn, scope, &q, None, DEDUPE_CANDIDATES)? {
        let have = token_set(&format!("{} {}", e.title, e.text));
        if jaccard(&want, &have) >= DEDUPE_JACCARD {
            return Ok(Some(e.id));
        }
    }
    Ok(None)
}

/// FTS5 search over live entries. Returns `(entry, relevance)` best first; relevance is
/// `-bm25` (higher is better, always >= 0). Title is weighted over tags over text.
pub(crate) fn search(
    conn: &Connection,
    scope: Scope,
    fts: &str,
    kind: Option<Kind>,
    limit: usize,
) -> Result<Vec<(Entry, f64)>, MemoryError> {
    let sql = format!(
        "SELECT {COLS}, bm25(memory_fts, 8.0, 1.0, 4.0) FROM memory_fts \
         JOIN memory_entries e ON e.seq = memory_fts.rowid \
         WHERE memory_fts MATCH ?1 AND {LIVE} AND (?2 IS NULL OR e.kind = ?2) \
         ORDER BY bm25(memory_fts, 8.0, 1.0, 4.0) LIMIT ?3"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let kind_s = kind.map(Kind::as_str);
    let rows = stmt.query_map(params![fts, kind_s, limit as i64], |r| {
        let rel: f64 = r.get(12)?;
        Ok((row_entry(scope, r)?, (-rel).max(0.0)))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Live entries: pinned first, then most recently updated.
pub(crate) fn recent(
    conn: &Connection,
    scope: Scope,
    kind: Option<Kind>,
    limit: usize,
) -> Result<Vec<Entry>, MemoryError> {
    let sql = format!(
        "SELECT {COLS} FROM memory_entries e WHERE {LIVE} AND (?1 IS NULL OR e.kind = ?1) \
         ORDER BY e.pinned DESC, e.updated_ms DESC, e.id DESC LIMIT ?2"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![kind.map(Kind::as_str), limit as i64], |r| {
        row_entry(scope, r)
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn get_live(
    conn: &Connection,
    scope: Scope,
    id: &str,
) -> Result<Option<Entry>, MemoryError> {
    let sql = format!("SELECT {COLS} FROM memory_entries e WHERE e.id = ?1 AND {LIVE}");
    conn.query_row(&sql, [id], |r| row_entry(scope, r))
        .optional()
        .map_err(Into::into)
}

/// Soft delete. Returns whether a live entry was marked deleted.
pub(crate) fn soft_delete(conn: &Connection, id: &str, now: i64) -> Result<bool, MemoryError> {
    let n = conn.execute(
        "UPDATE memory_entries SET deleted_ms = ?2 \
         WHERE id = ?1 AND deleted_ms IS NULL AND superseded_by IS NULL",
        params![id, now],
    )?;
    Ok(n > 0)
}

/// Bump `last_recalled_ms` / `recall_count` (one transaction). Does not touch FTS.
pub(crate) fn touch(conn: &Connection, ids: &[String], now: i64) -> Result<(), MemoryError> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "UPDATE memory_entries SET last_recalled_ms = ?2, recall_count = recall_count + 1 \
             WHERE id = ?1",
        )?;
        for id in ids {
            stmt.execute(params![id, now])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Prompt snapshot: pinned (oldest first, stable prefix) and the most recently updated
/// non-pinned entries.
pub(crate) fn snapshot(
    conn: &Connection,
    scope: Scope,
    pinned_cap: usize,
    recent_cap: usize,
) -> Result<(Vec<Entry>, Vec<Entry>), MemoryError> {
    let q_pinned = format!(
        "SELECT {COLS} FROM memory_entries e WHERE {LIVE} AND e.pinned = 1 \
         ORDER BY e.created_ms ASC, e.id ASC LIMIT ?1"
    );
    let q_recent = format!(
        "SELECT {COLS} FROM memory_entries e WHERE {LIVE} AND e.pinned = 0 \
         ORDER BY e.updated_ms DESC, e.id DESC LIMIT ?1"
    );
    let mut s1 = conn.prepare_cached(&q_pinned)?;
    let pinned = s1
        .query_map([pinned_cap as i64], |r| row_entry(scope, r))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut s2 = conn.prepare_cached(&q_recent)?;
    let recent = s2
        .query_map([recent_cap as i64], |r| row_entry(scope, r))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((pinned, recent))
}

pub(crate) fn count_all(conn: &Connection) -> Result<i64, MemoryError> {
    conn.query_row("SELECT COUNT(*) FROM memory_entries", [], |r| r.get(0))
        .map_err(Into::into)
}

/// Hard-delete forgotten / superseded entries last touched before `cutoff_ms`
/// (cascades to embeddings, FTS rows follow via trigger). Returns rows removed.
pub(crate) fn purge_before(conn: &Connection, cutoff_ms: i64) -> Result<usize, MemoryError> {
    let n = conn.execute(
        "DELETE FROM memory_entries WHERE (deleted_ms IS NOT NULL AND deleted_ms < ?1) \
         OR (superseded_by IS NOT NULL AND updated_ms < ?1)",
        [cutoff_ms],
    )?;
    Ok(n)
}
