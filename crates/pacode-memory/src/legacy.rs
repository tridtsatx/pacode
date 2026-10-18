//! One-shot import of the old flat `memory.md` (`- bullet` lines).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};

use crate::error::MemoryError;
use crate::id::new_id;
use crate::ops;
use crate::types::{Kind, NewEntry};

/// Only the first bullets are pinned (they are injected into the prompt in full).
pub const MAX_PINNED_IMPORT: usize = 20;

/// Bullet texts of a legacy file, in order.
pub fn parse_bullets(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let body = l.strip_prefix("- ").or_else(|| l.strip_prefix("* "))?;
            let body = body.trim();
            (!body.is_empty()).then(|| body.to_string())
        })
        .collect()
}

pub fn migrated_path(md: &Path) -> PathBuf {
    let mut name = md.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".migrated");
    md.with_file_name(name)
}

/// Import when the database holds zero rows, then rename the file to
/// `memory.md.migrated` (never deleted). The check and the inserts share one
/// `BEGIN IMMEDIATE` transaction so concurrent processes import once.
/// Returns the number of imported entries.
pub(crate) fn import(conn: &mut Connection, md: &Path, now_ms: i64) -> Result<usize, MemoryError> {
    let content = std::fs::read_to_string(md)?;
    let bullets = parse_bullets(&content);
    let mut imported = 0;
    let mut was_empty = false;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if ops::count_all(&tx)? == 0 {
        was_empty = true;
        let base = now_ms - bullets.len() as i64;
        for (i, b) in bullets.iter().enumerate() {
            let mut n = NewEntry::new(Kind::Fact, "", b.as_str());
            n.pinned = i < MAX_PINNED_IMPORT;
            n.source = Some("migrated:memory.md".to_string());
            let n = n.normalize()?;
            ops::insert(&tx, &new_id(base + i as i64), &n, base + i as i64)?;
            imported += 1;
        }
    }
    tx.commit()?;
    // A file next to a non-empty database is left alone (the user may have restored it).
    if was_empty
        && let Err(e) = std::fs::rename(md, migrated_path(md))
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("could not rename {}: {e}", md.display());
    }
    Ok(imported)
}
