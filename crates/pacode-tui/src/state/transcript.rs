//! Transcript cells with a bounded render cache and paced streaming.
//!
//! This module holds the transcript and how items land in it (`upsert`, the
//! client's own rows, permission prompts). The rest is split by concern:
//! `transcript_cell` (the cell types and their byte weight),
//! `transcript_evict` (cell and byte caps, history pages),
//! `transcript_stream` (paced reveal, tool output) and `transcript_view`
//! (scroll, rows under the pointer, expansion and exploration groups).

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use pacode_render::{LineCache, RenderOptions, StreamBuffer, StreamKind};

use crate::ui::toolgroup::GroupHead;
use pacode_types::{PermissionId, PermissionRequest, ToolStatus, TranscriptItem, TranscriptKind};

#[path = "tool_stream.rs"]
mod tool_stream;
#[path = "transcript_cell.rs"]
mod transcript_cell;
#[path = "transcript_evict.rs"]
mod transcript_evict;
#[path = "transcript_stream.rs"]
mod transcript_stream;
#[path = "transcript_view.rs"]
mod transcript_view;

pub use tool_stream::{MAX_TOOL_STREAM_CHARS, MAX_TOOL_STREAMS, ToolStreamView, ToolStreams};
use transcript_cell::cell_bytes;
pub use transcript_cell::{
    BackgroundKind, BackgroundOutcome, BackgroundResult, Cell, CellKind, HeaderInfo, PermExplain,
    explain_level_name,
};

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod transcript_tests;

pub(crate) type StableRevision = (u64, usize);

/// Ceiling on the bytes one transcript holds client-side. The cell cap counts
/// rows, not weight: a hundred tool calls with 2 MiB previews pass any cell cap
/// and still pin hundreds of megabytes. The daemon's own tail is capped at
/// 4 MiB per session, so this leaves room for a full session of growth before
/// the oldest cells start dropping.
pub const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024 * 1024;

/// The high bit of a cell id marks it as the client's own. Daemon item seqs
/// are a per-session counter starting at zero; client rows (notices, gaps,
/// finished background jobs) used to be keyed by a wall clock or by an event
/// seq, and both alias that counter: two notices in the same millisecond
/// replaced each other, a permission row could land on top of a transcript
/// item, and one evicted client row pushed the eviction floor past every real
/// seq so later items were dropped as "already evicted".
const CLIENT_ID_TAG: u64 = 1 << 63;

fn is_client_id(id: u64) -> bool {
    id & CLIENT_ID_TAG != 0
}

pub(crate) struct RenderProfile {
    pub(crate) width: u16,
    pub(crate) options: RenderOptions,
    pub(crate) anim_frame: u64,
}

pub struct Transcript {
    pub cells: VecDeque<Cell>,
    /// Non-persisted banner drawn above the first cell. Kept out of `cells` so a
    /// transcript item can never collide with it by seq.
    pub header: Option<HeaderInfo>,
    /// Bumped on every header change so the render cache can key on it.
    pub header_version: u32,
    pub max_cells: usize,
    /// Ceiling on the estimated resident bytes of the cells; see
    /// `MAX_TRANSCRIPT_BYTES`.
    pub max_bytes: usize,
    /// Estimated resident bytes per cell id, kept in step with `bytes` by every
    /// path that adds, replaces or drops a cell.
    cell_bytes: HashMap<u64, usize>,
    bytes: usize,
    /// Scroll offset in rendered lines from the bottom; 0 = follow the tail.
    pub scroll_from_bottom: usize,
    /// Total rendered line count recorded by the most recent draw. `None` before first draw.
    pub rendered_lines: Option<usize>,
    /// Viewport height recorded by the most recent draw. `None` before first draw.
    pub viewport_height: Option<usize>,
    /// Index of the content line drawn at the top of the viewport by the most
    /// recent draw. Selection is anchored to content lines, so this is what maps
    /// a screen row to the line the reader actually clicked on.
    pub first_visible_line: usize,
    /// Cells whose output is shown in full instead of the first few lines.
    pub expanded: HashSet<u64>,
    /// Which content lines belong to which cell, from the most recent draw:
    /// `(cell id, first line, one past the last)`. This is how a click on a row
    /// finds the cell under it.
    pub cell_lines: Vec<(u64, usize, usize)>,
    /// The header rows of every exploration group of the most recent draw: a
    /// click there opens the whole group rather than one call of it.
    pub(crate) group_heads: Vec<GroupHead>,
    pub cache: LineCache,
    pub(crate) stable_revisions: HashMap<u64, StableRevision>,
    pub(crate) cell_line_index: HashMap<u64, usize>,
    /// Monotonic source of client-allocated cell ids; survives `reset` so an id
    /// is never handed out twice in one session.
    next_client_id: u64,
    evicted_floor: Option<u64>,
    render_profile: Option<RenderProfile>,
    /// Streaming reveal buffer for the live assistant cell (`live_cell`).
    pub stream: Option<StreamBuffer>,
    pub live_cell: Option<u64>,
    /// Output received so far for each running tool call, keyed by cell id.
    /// The wire item carries no partial output, so the live body of a running
    /// call lives here until that call's final item replaces it.
    pub tool_streams: ToolStreams,
    /// Final item received while the paced reveal still has backlog: applied when the
    /// backlog drains so the tail of an answer does not pop in at once.
    pub pending_final: Option<TranscriptItem>,
    /// Hidden text that arrived but is not revealed yet is inside `stream`.
    pub has_more_history: bool,
    /// Set while a `GetHistory` request is in flight.
    pub loading_history: bool,
}

impl Transcript {
    pub fn new(max_cells: usize) -> Self {
        Self {
            cells: VecDeque::new(),
            header: None,
            header_version: 0,
            max_cells,
            max_bytes: MAX_TRANSCRIPT_BYTES,
            cell_bytes: HashMap::new(),
            bytes: 0,
            scroll_from_bottom: 0,
            rendered_lines: None,
            viewport_height: None,
            first_visible_line: 0,
            expanded: HashSet::new(),
            cell_lines: Vec::new(),
            group_heads: Vec::new(),
            cache: LineCache::new(20_000),
            stable_revisions: HashMap::new(),
            cell_line_index: HashMap::new(),
            next_client_id: 0,
            evicted_floor: None,
            render_profile: None,
            stream: None,
            live_cell: None,
            tool_streams: ToolStreams::default(),
            pending_final: None,
            has_more_history: false,
            loading_history: false,
        }
    }

    /// Replace everything (snapshot).
    pub fn reset(&mut self, items: Vec<TranscriptItem>, has_more: bool) {
        self.cells.clear();
        self.cache.clear();
        self.stable_revisions.clear();
        self.cell_line_index.clear();
        self.cell_bytes.clear();
        self.bytes = 0;
        self.evicted_floor = None;
        self.render_profile = None;
        self.stream = None;
        self.live_cell = None;
        self.tool_streams.clear();
        self.pending_final = None;
        self.expanded.clear();
        self.cell_lines.clear();
        self.group_heads.clear();
        self.scroll_from_bottom = 0;
        self.first_visible_line = 0;
        self.loading_history = false;
        self.rendered_lines = None;
        self.viewport_height = None;
        self.has_more_history = has_more;

        for item in items {
            self.append_indexed(Cell {
                id: item.seq,
                kind: CellKind::Item(item.kind),
                version: 0,
                ts_ms: item.ts_ms,
                stats: None,
                perm_explain: None,
            });
        }
        self.enforce_limits();
    }

    /// Estimated resident bytes of the cells on screen.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn set_render_profile(
        &mut self,
        width: u16,
        options: &RenderOptions,
        anim_frame: u64,
    ) {
        self.render_profile = Some(RenderProfile {
            width,
            options: options.clone(),
            anim_frame,
        });
    }

    /// Where a cell sits in `cells`. A scan rather than an id→position map:
    /// `cells` is a public deque, and every map that claimed to mirror it went
    /// stale the moment something else touched it — evicting one row and
    /// appending another leaves the length identical, and a lookup that trusted
    /// the stale map appended a second copy of a row already on screen.
    fn cell_index(&self, cell_id: u64) -> Option<usize> {
        self.cells.iter().position(|cell| cell.id == cell_id)
    }

    fn set_cell_bytes(&mut self, id: u64, size: usize) {
        self.bytes = self
            .bytes
            .saturating_sub(self.cell_bytes.remove(&id).unwrap_or(0))
            .saturating_add(size);
        self.cell_bytes.insert(id, size);
    }

    fn forget_bytes(&mut self, id: u64) -> usize {
        let size = self.cell_bytes.remove(&id).unwrap_or(0);
        self.bytes = self.bytes.saturating_sub(size);
        size
    }

    /// A client id no daemon item could carry: monotonic, and never handed out
    /// twice in one session.
    fn allocate_client_id(&mut self) -> u64 {
        loop {
            self.next_client_id = self.next_client_id.wrapping_add(1) & !CLIENT_ID_TAG;
            if self.next_client_id != 0 {
                return CLIENT_ID_TAG | self.next_client_id;
            }
        }
    }

    fn append_indexed(&mut self, cell: Cell) {
        let size = cell_bytes(&cell);
        if let Some(index) = self.cell_index(cell.id) {
            self.set_cell_bytes(cell.id, size);
            self.cells[index] = cell;
            return;
        }
        self.set_cell_bytes(cell.id, size);
        self.cells.push_back(cell);
    }

    /// Set or update the non-persisted header banner.
    pub fn set_header(&mut self, info: HeaderInfo) {
        if self.header.as_ref() == Some(&info) {
            return;
        }
        self.header = Some(info);
        self.header_version = self.header_version.wrapping_add(1);
    }

    /// `ItemAdded` / `ItemUpdated`: insert or replace by seq. Assistant/Reasoning items
    /// that are not complete become the live cell with an empty revealed text.
    pub fn upsert(&mut self, item: TranscriptItem, now: Instant) {
        let seq = item.seq;
        if self.is_evicted(seq) {
            return;
        }
        match item.kind {
            TranscriptKind::Assistant { text, complete } => {
                if !complete {
                    let kind = TranscriptKind::Assistant {
                        text: String::new(),
                        complete: false,
                    };
                    self.insert_or_replace(seq, kind, item.ts_ms);
                    self.set_live_cell(seq, now);
                    if !text.is_empty()
                        && let Some(sb) = self.stream.as_mut()
                    {
                        sb.push(StreamKind::Text, &text);
                    }
                } else {
                    if self.live_cell == Some(seq) && self.has_backlog() {
                        self.pending_final = Some(TranscriptItem {
                            seq,
                            agent: item.agent,
                            ts_ms: item.ts_ms,
                            kind: TranscriptKind::Assistant {
                                text,
                                complete: true,
                            },
                        });
                        return;
                    }
                    if self.live_cell == Some(seq) {
                        self.live_cell = None;
                        self.stream = None;
                    }
                    let kind = TranscriptKind::Assistant {
                        text,
                        complete: true,
                    };
                    self.insert_or_replace(seq, kind, item.ts_ms);
                }
            }
            TranscriptKind::Reasoning { text, complete } => {
                if !complete {
                    let kind = TranscriptKind::Reasoning {
                        text: String::new(),
                        complete: false,
                    };
                    self.insert_or_replace(seq, kind, item.ts_ms);
                    self.set_live_cell(seq, now);
                    if !text.is_empty()
                        && let Some(sb) = self.stream.as_mut()
                    {
                        sb.push(StreamKind::Reasoning, &text);
                    }
                } else {
                    if self.live_cell == Some(seq) && self.has_backlog() {
                        self.pending_final = Some(TranscriptItem {
                            seq,
                            agent: item.agent,
                            ts_ms: item.ts_ms,
                            kind: TranscriptKind::Reasoning {
                                text,
                                complete: true,
                            },
                        });
                        return;
                    }
                    if self.live_cell == Some(seq) {
                        self.live_cell = None;
                        self.stream = None;
                    }
                    let kind = TranscriptKind::Reasoning {
                        text,
                        complete: true,
                    };
                    self.insert_or_replace(seq, kind, item.ts_ms);
                }
            }
            other => {
                // A terminal cell must never land mid-reveal: the live cell can
                // still hold buffered text that precedes this item on the wire,
                // and appending first would split the answer around the tool.
                // Flushing preserves wire order. Updates to an existing cell
                // replace in place and cannot reorder, so they skip the flush.
                if self.cell_index(seq).is_none() {
                    self.flush_stream();
                }
                self.insert_or_replace(seq, other, item.ts_ms);
            }
        }
        self.enforce_limits();
    }

    /// Append a row the client invented (a notice, a gap, a finished
    /// background job). Its identity is allocated here rather than taken from
    /// the caller: callers hand over a wall clock or an event seq, and keying a
    /// cell by either puts it in the daemon's seq space, where it can overwrite
    /// a transcript item.
    pub fn push_cell(&mut self, mut cell: Cell) {
        if self.has_backlog() || self.has_pending_final() {
            self.flush_stream();
        }
        cell.id = self.allocate_client_id();
        self.append_indexed(cell);
        self.enforce_limits();
    }

    /// Index of the prompt row carrying `permission`, if it is still on screen.
    fn permission_cell_index(&self, permission: &PermissionId) -> Option<usize> {
        self.cells.iter().position(|cell| {
            matches!(
                &cell.kind,
                CellKind::Item(TranscriptKind::Permission(existing)) if &existing.id == permission
            )
        })
    }

    fn refresh_permission_cell(&mut self, index: usize, req: PermissionRequest, ts_ms: u64) {
        let cell = &mut self.cells[index];
        cell.kind = CellKind::Item(TranscriptKind::Permission(req));
        cell.version = cell.version.wrapping_add(1);
        cell.ts_ms = ts_ms;
    }

    /// Insert or refresh the pending-permission prompt for `req`, keyed on the
    /// permission id rather than a seq: the daemon emits the same request
    /// twice (`PermissionRequested`, then an `ItemAdded` transcript item) and
    /// a snapshot repeats it via `pending_permissions`. The first write keeps
    /// the slot — later writes refresh the request in place, so the prompt
    /// never renders twice and any `perm_explain` state survives the
    /// duplicate. A prompt that arrives as an event has no transcript item seq
    /// to key on, so it is filed under a client id (see `push_cell`).
    pub fn upsert_permission(&mut self, req: PermissionRequest, ts_ms: u64, _fallback_id: u64) {
        if let Some(index) = self.permission_cell_index(&req.id) {
            self.refresh_permission_cell(index, req, ts_ms);
            return;
        }
        self.push_cell(Cell {
            id: 0,
            kind: CellKind::Item(TranscriptKind::Permission(req)),
            version: 0,
            ts_ms,
            stats: None,
            perm_explain: None,
        });
    }

    /// Fold an `e`-key explanation state into every cell still carrying the
    /// pending request — the `PermissionRequested` row and the transcript
    /// item share the request's id. Returns true when at least one live
    /// prompt took it; false means the request already resolved and its
    /// prompt is gone.
    pub fn set_perm_explain(&mut self, permission: &PermissionId, explain: &PermExplain) -> bool {
        let mut landed = false;
        for cell in &mut self.cells {
            if matches!(
                &cell.kind,
                CellKind::Item(TranscriptKind::Permission(req)) if req.id == *permission
            ) {
                cell.perm_explain = Some(explain.clone());
                cell.version = cell.version.wrapping_add(1);
                landed = true;
            }
        }
        landed
    }

    fn insert_or_replace(&mut self, seq: u64, kind: TranscriptKind, ts_ms: u64) {
        // A call that is no longer running owns its body: the daemon's final
        // preview replaces the live bytes, and keeping them would hold output
        // that is no longer on screen.
        if matches!(
            kind,
            TranscriptKind::ToolCall {
                status: ToolStatus::Ok
                    | ToolStatus::Error
                    | ToolStatus::Backgrounded
                    | ToolStatus::Denied,
                ..
            }
        ) {
            self.tool_streams.remove(seq);
        }

        // A permission item is the same prompt `PermissionRequested` already
        // rendered — keyed by request id so the daemon's double emission
        // lands on one cell instead of prompting twice. When the prompt is not
        // on screen yet, the item's own seq is the right key: a history page
        // that repeats this item has to find the cell it already made.
        if let TranscriptKind::Permission(req) = kind {
            if let Some(index) = self.permission_cell_index(&req.id) {
                self.refresh_permission_cell(index, req, ts_ms);
                return;
            }
            if self.is_evicted(seq) {
                return;
            }
            self.append_indexed(Cell {
                id: seq,
                kind: CellKind::Item(TranscriptKind::Permission(req)),
                version: 0,
                ts_ms,
                stats: None,
                perm_explain: None,
            });
            return;
        }
        if self.is_evicted(seq) {
            return;
        }
        let (version, stats, perm_explain) = self
            .cell_index(seq)
            .map(|index| {
                let existing = &self.cells[index];
                (
                    existing.version.wrapping_add(1),
                    existing.stats.clone(),
                    existing.perm_explain.clone(),
                )
            })
            .unwrap_or((0, None, None));
        self.append_indexed(Cell {
            id: seq,
            kind: CellKind::Item(kind),
            version,
            ts_ms,
            stats,
            perm_explain,
        });
        let stable_len = self
            .cell_index(seq)
            .and_then(|index| self.cells.get(index))
            .and_then(Self::stable_prefix_len);
        self.refresh_stable_revision(seq, stable_len, true);
    }
}
