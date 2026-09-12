//! Touched files tracking for the files overlay and the `Files` menu level
//! (spec §11, §5.5).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use pacode_types::{CallId, DiffStat};

pub use pacode_vcs::FileStatus;

#[cfg(test)]
#[path = "files_tests.rs"]
mod files_tests;

pub const MAX_FILES: usize = 500;

/// Bytes of a text file the deferred preview probe reads — enough for the
/// detail card's head; the file is not slurped whole (§8.1, §6.4).
pub const PREVIEW_BYTES: usize = 64 * 1024;
/// Lines of the head kept after lexing — the card shows what fits.
pub const PREVIEW_LINES: usize = 100;

/// Key identifying a cached preview by the *raw* path (as the files row lists
/// it — possibly relative to the session cwd). The background job resolves
/// the raw path against the cwd; draw keys on the raw string so it never has
/// to stat anything (§8.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewKey {
    pub raw: String,
}

/// One syntax-lexed line of a text preview.
#[derive(Clone, Debug)]
pub struct PreviewLine {
    pub text: String,
    /// `syntax::Token`s tiling `text` — empty for unknown languages (the line
    /// still renders, unhighlighted).
    pub toks: Vec<pacode_render::syntax::Token>,
}

/// The head of a text file, lexed on the blocking thread — the detail card
/// only paints tokens, never reads or lexes in draw (§8.1, §7.2).
#[derive(Clone, Debug)]
pub struct TextPreview {
    pub lines: Vec<PreviewLine>,
    /// The file continues past what the probe read.
    pub truncated: bool,
}

/// Cache entry for at most one rendered preview.
/// Owner: `FilesState`.
/// Cap: exactly 1 entry.
/// Invalidation rule: dropped when the Files menu level closes or the
/// highlighted entry changes.
#[derive(Clone, Debug)]
pub struct CachedPreview {
    pub key: PreviewKey,
    /// The cwd-resolved path the preview was produced from.
    pub resolved: PathBuf,
    /// `None` when the file is binary, an image, unreadable, or absent.
    pub text: Option<TextPreview>,
}

/// Bitflags for operations performed on a file: Read, Write, Edit, Search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileKinds(pub u8);

impl FileKinds {
    pub const READ: u8 = 1 << 0;
    pub const WRITE: u8 = 1 << 1;
    pub const EDIT: u8 = 1 << 2;
    pub const SEARCH: u8 = 1 << 3;

    pub fn new() -> Self {
        Self(0)
    }

    pub fn add(&mut self, flag: u8) {
        self.0 |= flag;
    }

    pub fn contains(&self, flag: u8) -> bool {
        (self.0 & flag) != 0
    }

    pub fn has_read(&self) -> bool {
        self.contains(Self::READ)
    }

    pub fn has_write(&self) -> bool {
        self.contains(Self::WRITE)
    }

    pub fn has_edit(&self) -> bool {
        self.contains(Self::EDIT)
    }

    pub fn has_search(&self) -> bool {
        self.contains(Self::SEARCH)
    }
}

/// One row in the files table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub path: String,
    pub kinds: FileKinds,
    pub count: u32,
    pub last_ts_ms: u64,
    /// `ToolCall.diff` sums folded in by [`FilesState::apply_diff`] — the
    /// `+A −R` of the row and its directory aggregate.
    pub added: u32,
    pub removed: u32,
    /// Any modifying call carried a `DiffStat` — gates the `created` heuristic
    /// and the stats tail.
    pub has_diff: bool,
    /// The first observed write/edit diff had only additions — the file did
    /// not exist before this session (a `git status` `New` wins when present).
    pub created: bool,
    /// CallIds already folded into `added`/`removed` — an `ItemUpdated` can
    /// repeat a completed call, and stats must not double-count.
    diff_calls: HashSet<CallId>,
}

/// State tracking files touched by tools during the session.
#[derive(Clone, Debug, Default)]
pub struct FilesState {
    pub rows: Vec<FileRow>,
    /// At most one cached preview (image or lexed text).
    pub cached_preview: Option<CachedPreview>,
    /// What the preview needs — the draw path records it, the event loop turns
    /// it into `preview_in_flight` and a blocking-thread job. Rendering inside
    /// draw was the §8.1 violation: file IO, decode/encode and lexing are all
    /// synchronous work.
    pub preview_want: Option<PreviewKey>,
    /// The key of the preview job currently running on a blocking thread.
    pub preview_in_flight: Option<PreviewKey>,
    /// `git status` scan results keyed by the exact row path spelling (the
    /// scan maps git's root-relative output back to the input) — the
    /// `new`/`deleted`/`renamed` row labels (§5.5).
    pub statuses: BTreeMap<String, FileStatus>,
    /// The `rows_version` the last dispatched status scan ran against —
    /// `None` = never scanned (or invalidated on menu close). Cleared by the
    /// event loop when no files view is on screen, so reopening rescans.
    pub statused_at: Option<u64>,
    /// A status scan is on a background task.
    pub statuses_in_flight: bool,
    /// Bumped on every row or diff-stat mutation — the staleness signal for
    /// `statused_at`.
    pub rows_version: u64,
    /// The path `enter` armed a worktree-diff probe for — the event loop turns
    /// it into `diff_in_flight` and `BgResponse::FileDiffReady`.
    pub diff_want: Option<String>,
    /// The path a `file_diff` job is currently producing.
    pub diff_in_flight: Option<String>,
}

impl FilesState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear cached preview and outstanding wants (e.g. on menu close).
    pub fn clear_preview(&mut self) {
        self.cached_preview = None;
        self.preview_want = None;
        self.preview_in_flight = None;
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// `(files, added, removed)` — the root menu's `Files` row summary.
    pub fn totals(&self) -> (usize, u32, u32) {
        let added = self.rows.iter().map(|r| r.added).sum();
        let removed = self.rows.iter().map(|r| r.removed).sum();
        (self.rows.len(), added, removed)
    }

    /// The worktree status the deferred `git status` scan reported for this
    /// row — `statuses` is keyed by the exact row spelling (the scan resolves
    /// root-relative output back to it), so this is a plain lookup (§5.5).
    pub fn status_of(&self, path: &str) -> Option<FileStatus> {
        self.statuses.get(path).copied()
    }

    /// The scan result lands whole: replace the map, record which
    /// `rows_version` it described.
    pub fn apply_statuses(&mut self, statuses: BTreeMap<String, FileStatus>, at_version: u64) {
        self.statuses = statuses;
        self.statused_at = Some(at_version);
        self.statuses_in_flight = false;
    }

    /// Fold one finished call's `DiffStat` into the row — `ItemUpdated`
    /// carries it (the row itself was created by `ItemAdded`), and so does the
    /// attach snapshot. Deduped per call id: the same completed item can be
    /// reported more than once.
    pub fn apply_diff(
        &mut self,
        tool_name: &str,
        input: &serde_json::Value,
        call_id: &CallId,
        diff: DiffStat,
        ts_ms: u64,
    ) {
        if tool_name.contains("__")
            || !matches!(
                tool_name,
                "write_file" | "write" | "edit_file" | "edit" | "apply_patch" | "multi_edit"
            )
        {
            return;
        }
        for path in extract_paths(input) {
            if path.is_empty() {
                continue;
            }
            if let Some(row) = self.rows.iter_mut().find(|r| r.path == path)
                && row.diff_calls.insert(call_id.clone())
            {
                row.added = row.added.saturating_add(diff.added);
                row.removed = row.removed.saturating_add(diff.removed);
                row.last_ts_ms = row.last_ts_ms.max(ts_ms);
                if !row.has_diff && diff.added > 0 && diff.removed == 0 {
                    row.created = true;
                }
                row.has_diff = true;
                self.rows_version = self.rows_version.wrapping_add(1);
            }
        }
    }

    /// Observe a tool item call and update tracked files.
    /// MCP tools (containing `__`) are ignored.
    /// Tool names:
    /// - `read_file` / `read` -> R
    /// - `write_file` / `write` -> W
    /// - `edit_file` / `edit` / `apply_patch` / `multi_edit` -> E
    /// - `grep` / `glob` / `search` / `ls` -> S
    pub fn observe_tool_item(&mut self, tool_name: &str, input: &serde_json::Value, ts_ms: u64) {
        if tool_name.contains("__") {
            return;
        }

        let kind_flag = match tool_name {
            "read_file" | "read" => FileKinds::READ,
            "write_file" | "write" => FileKinds::WRITE,
            "edit_file" | "edit" | "apply_patch" | "multi_edit" => FileKinds::EDIT,
            "grep" | "glob" | "search" | "ls" => FileKinds::SEARCH,
            _ => return,
        };

        let paths = extract_paths(input);
        for path in paths {
            if path.is_empty() {
                continue;
            }
            if let Some(row) = self.rows.iter_mut().find(|r| r.path == path) {
                row.kinds.add(kind_flag);
                row.count = row.count.saturating_add(1);
                row.last_ts_ms = ts_ms;
            } else {
                let mut kinds = FileKinds::new();
                kinds.add(kind_flag);
                self.rows.push(FileRow {
                    path,
                    kinds,
                    count: 1,
                    last_ts_ms: ts_ms,
                    added: 0,
                    removed: 0,
                    has_diff: false,
                    created: false,
                    diff_calls: HashSet::new(),
                });
                self.enforce_cap();
            }
            self.rows_version = self.rows_version.wrapping_add(1);
        }
    }

    fn enforce_cap(&mut self) {
        while self.rows.len() > MAX_FILES {
            if let Some((min_idx, _)) = self
                .rows
                .iter()
                .enumerate()
                .min_by_key(|(_, r)| r.last_ts_ms)
            {
                self.rows.remove(min_idx);
            } else {
                break;
            }
        }
    }
}

/// The path argument a `ToolCall`'s title carries (`Write src/x.rs` →
/// `src/x.rs`) — the transcript keeps no structured input, so both the
/// live-event path and the attach snapshot extract it the same way here.
pub fn title_path_arg<'a>(name: &str, title: &'a str) -> &'a str {
    title
        .strip_prefix(name)
        .map(str::trim_start)
        .unwrap_or_else(|| {
            title
                .split_once(' ')
                .map(|(_, r)| r.trim())
                .unwrap_or(title)
        })
}

fn extract_paths(input: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(obj) = input.as_object() else {
        return out;
    };

    for key in ["path", "file_path", "dir"] {
        if let Some(val) = obj.get(key)
            && let Some(s) = val.as_str()
            && !s.is_empty()
        {
            out.push(s.to_string());
        }
    }

    if let Some(val) = obj.get("paths") {
        if let Some(arr) = val.as_array() {
            for item in arr {
                if let Some(s) = item.as_str()
                    && !s.is_empty()
                {
                    out.push(s.to_string());
                }
            }
        } else if let Some(s) = val.as_str()
            && !s.is_empty()
        {
            out.push(s.to_string());
        }
    }

    out
}
