//! After-write mode: diff the file before/after an edit/write tool call and scan
//! the added lines.

use std::path::PathBuf;
use std::sync::Arc;

use pacode_rules::{ModeFilter, Severity, render_after_write, scan_added};

use super::{admit, emit_fired, ruleset_for};
use crate::agent::Agent;
use crate::session::Session;

/// Largest file read for scanning; bigger files are skipped.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const GUARDED_TOOLS: [&str; 3] = ["write", "edit", "multi_edit"];

/// The file as it was before the tool ran. Lives for one tool call.
pub(crate) struct Snapshot {
    /// Absolute path.
    path: PathBuf,
    /// Path relative to the session cwd (what rule `exclude` patterns see).
    rel: PathBuf,
    /// `None` = the file did not exist (every line is new).
    old: Option<String>,
}

/// What to append to the tool result.
pub(crate) struct Found {
    pub(crate) block: String,
    pub(crate) is_error: bool,
}

async fn read_capped(path: &std::path::Path) -> Option<Option<String>> {
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.len() > MAX_FILE_BYTES => None,
        Ok(_) => Some(
            tokio::fs::read(path)
                .await
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok()),
        ),
        Err(_) => Some(None),
    }
}

/// Capture the pre-call state. `None` when the tool is not a file writer, the guard
/// is off, no rule applies to this file, or the file is too large.
pub(crate) async fn snapshot(
    session: &Arc<Session>,
    tool: &str,
    input: &serde_json::Value,
) -> Option<Snapshot> {
    if !GUARDED_TOOLS.contains(&tool) {
        return None;
    }
    let raw = input.get("path")?.as_str()?;
    let set = ruleset_for(session).await?;
    let cwd = session.meta().cwd.clone();
    let path = cwd.join(raw);
    let rel = path.strip_prefix(&cwd).unwrap_or(&path).to_path_buf();
    if set.applicable(&rel, ModeFilter::AfterWrite).is_empty() {
        return None;
    }
    let old = read_capped(&path).await?;
    Some(Snapshot { path, rel, old })
}

/// Scan the lines the finished call added. `None` = nothing to report.
pub(crate) async fn check_after_write(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    snap: Snapshot,
) -> Option<Found> {
    let set = ruleset_for(session).await?;
    let new = read_capped(&snap.path).await??;
    // The exact diff + regex scan of up to 2 MiB is CPU work: off the async workers.
    let Snapshot { rel, old, .. } = snap;
    let report = tokio::task::spawn_blocking(move || {
        scan_added(&set, &rel, old.as_deref(), &new, ModeFilter::AfterWrite)
    })
    .await
    .ok()?;
    if report.is_empty() {
        return None;
    }
    let path = report.path.clone();
    let suppressed = report.suppressed;
    let findings = admit(session, agent, report.findings, false);
    if findings.is_empty() {
        return None;
    }
    emit_fired(session, agent, &findings);
    Some(Found {
        block: render_after_write(&path, &findings, suppressed),
        is_error: findings.iter().any(|f| f.severity == Severity::Block),
    })
}
