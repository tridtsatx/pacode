//! Stream mode: scan write/edit tool arguments while they are still streaming and
//! cut the call short on a `mode: stream` hit.
//!
//! One [`StreamGuard`] lives for one `consume_stream` call. Per tool call it keeps
//! the decoded old file lines (cap: one file of at most 2 MB) and a scan watermark;
//! both are dropped with the guard. Re-scans are geometric (every 25% growth), so a
//! large write costs O(n) overall.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pacode_rules::{
    Mode, ModeFilter, OldLines, RuleSet, extract_string_fields, render_stream_stop, scan_added_fast,
};

use super::{admit, emit_fired, ruleset_for};
use crate::agent::Agent;
use crate::session::Session;

const STREAM_TOOLS: [&str; 3] = ["write", "edit", "multi_edit"];
/// Minimum argument growth between scans.
const MIN_SCAN_STEP: usize = 256;
/// Calls with larger arguments are left to the after-write scan.
const MAX_ARGS_BYTES: usize = 512 * 1024;
const MAX_OLD_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Default)]
struct CallScan {
    scanned_len: usize,
    old: Option<OldLines>,
    old_loaded: bool,
    done: bool,
}

/// A stream rule fired: `message` is the tool-error text for the cut-short call.
pub(crate) struct Hit {
    pub(crate) message: String,
}

pub(crate) struct StreamGuard {
    set: Arc<RuleSet>,
    cwd: PathBuf,
    calls: BTreeMap<u32, CallScan>,
}

impl StreamGuard {
    /// `None` when streaming checks are off or no `mode: stream` rule is loaded.
    pub(crate) async fn new(session: &Arc<Session>) -> Option<StreamGuard> {
        if !session.config.rules.stream {
            return None;
        }
        let set = ruleset_for(session).await?;
        if !set.rules().iter().any(|r| r.mode == Mode::Stream) {
            return None;
        }
        Some(StreamGuard {
            set,
            cwd: session.meta().cwd.clone(),
            calls: BTreeMap::new(),
        })
    }

    /// Look at the arguments streamed so far for call `index`.
    pub(crate) async fn check(
        &mut self,
        session: &Arc<Session>,
        agent: &Arc<Agent>,
        index: u32,
        tool: &str,
        args: &str,
    ) -> Option<Hit> {
        if !STREAM_TOOLS.contains(&tool) {
            return None;
        }
        let state = self.calls.entry(index).or_default();
        if state.done {
            return None;
        }
        if args.len() > MAX_ARGS_BYTES {
            state.done = true;
            return None;
        }
        if args.len() < state.scanned_len + MIN_SCAN_STEP.max(state.scanned_len / 4) {
            return None;
        }
        state.scanned_len = args.len();

        let text_key = if tool == "write" {
            "content"
        } else {
            "new_string"
        };
        let fields = extract_string_fields(args, &["path", text_key]);
        let raw_path = fields.iter().find(|f| f.key == "path" && f.complete)?;
        let abs = self.cwd.join(&raw_path.value);
        let rel = abs.strip_prefix(&self.cwd).unwrap_or(&abs).to_path_buf();
        if self.set.applicable(&rel, ModeFilter::StreamOnly).is_empty() {
            state.done = true;
            return None;
        }

        let parts: Vec<_> = fields.iter().filter(|f| f.key == text_key).collect();
        let last_incomplete = parts.last().is_some_and(|f| !f.complete);
        let mut text = parts
            .iter()
            .map(|f| f.value.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if last_incomplete {
            // Only whole lines: the last one may still be growing.
            let cut = text.rfind('\n')?;
            text.truncate(cut + 1);
        }
        if text.is_empty() {
            return None;
        }

        if !state.old_loaded {
            state.old_loaded = true;
            state.old = load_old_lines(&abs).await;
        }
        let Some(old) = state.old.as_ref() else {
            // Existing file too large / not UTF-8: leave it to the after-write scan.
            state.done = true;
            return None;
        };
        let report = scan_added_fast(&self.set, &rel, old, &text, ModeFilter::StreamOnly);
        if report.is_empty() {
            return None;
        }
        // Whatever happens next, this call has been judged.
        state.done = true;
        let kept = admit(session, agent, report.findings, true);
        if kept.is_empty() {
            return None;
        }
        emit_fired(session, agent, &kept);
        Some(Hit {
            message: render_stream_stop(tool, &report.path, &kept),
        })
    }
}

/// Lines of the file being replaced. A missing file is an empty set; `None` means
/// "cannot compare" (too large, unreadable or not UTF-8).
async fn load_old_lines(path: &Path) -> Option<OldLines> {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return Some(OldLines::default());
    };
    if meta.len() > MAX_OLD_FILE_BYTES {
        return None;
    }
    let bytes = tokio::fs::read(path).await.ok()?;
    Some(OldLines::new(&String::from_utf8(bytes).ok()?))
}
