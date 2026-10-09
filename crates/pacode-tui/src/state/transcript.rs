//! Transcript cells with a bounded render cache and paced streaming.

use std::collections::{HashMap, HashSet, VecDeque};
use std::mem::size_of;
use std::time::Instant;

use pacode_render::{LineCache, RenderOptions, StreamBuffer, StreamKind, StreamOp};
use pacode_types::{
    ExplainLevel, PermissionId, PermissionRequest, Question, ToolStatus, TranscriptItem,
    TranscriptKind,
};

#[path = "tool_stream.rs"]
mod tool_stream;

pub use tool_stream::{MAX_TOOL_STREAM_CHARS, MAX_TOOL_STREAMS, ToolStreamView, ToolStreams};

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

fn sum_bytes(values: impl IntoIterator<Item = usize>) -> usize {
    values.into_iter().fold(0usize, usize::saturating_add)
}

fn string_bytes(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::capacity)
}

fn question_bytes(question: &Question) -> usize {
    let options = question.options.iter().map(|option| {
        sum_bytes([
            size_of::<pacode_types::QuestionOption>(),
            option.label.capacity(),
            option.description.capacity(),
        ])
    });
    sum_bytes(
        [
            size_of::<Question>(),
            question.header.capacity(),
            question.question.capacity(),
        ]
        .into_iter()
        .chain(options),
    )
}

fn item_bytes(kind: &TranscriptKind) -> usize {
    match kind {
        TranscriptKind::User { text }
        | TranscriptKind::Assistant { text, .. }
        | TranscriptKind::Reasoning { text, .. } => text.capacity(),
        TranscriptKind::ToolCall {
            call_id,
            name,
            title,
            intent,
            preview,
            task,
            ..
        } => sum_bytes([
            call_id.as_str().len(),
            name.capacity(),
            title.capacity(),
            string_bytes(intent),
            preview.capacity(),
            task.as_ref().map_or(0, |id| id.as_str().len()),
        ]),
        TranscriptKind::Notice { text, .. } => text.capacity(),
        TranscriptKind::Permission(request) => sum_bytes([
            request.id.as_str().len(),
            request.agent.as_str().len(),
            request.agent_name.capacity(),
            request.call_id.as_str().len(),
            request.tool.capacity(),
            request.title.capacity(),
            request.detail.capacity(),
        ]),
        TranscriptKind::Question { question, .. } => question_bytes(question),
        TranscriptKind::BashCommand {
            command, output, ..
        } => sum_bytes([command.capacity(), output.capacity()]),
    }
}

/// Resident cost of one cell, estimated the way the daemon estimates its tail:
/// string capacities, not lengths, because capacity is what the process holds.
fn cell_bytes(cell: &Cell) -> usize {
    let kind = match &cell.kind {
        CellKind::Gap => 0,
        CellKind::Item(kind) => item_bytes(kind),
        CellKind::BackgroundResult(result) => {
            sum_bytes([size_of::<BackgroundResult>(), result.label.capacity()])
        }
    };
    sum_bytes([
        size_of::<Cell>(),
        kind,
        string_bytes(&cell.stats),
        match cell.perm_explain.as_ref() {
            Some(PermExplain::Requested) => 0,
            Some(PermExplain::Done { text, .. }) => text.capacity(),
            None => 0,
        },
    ])
}

pub(crate) struct RenderProfile {
    pub(crate) width: u16,
    pub(crate) options: RenderOptions,
    pub(crate) anim_frame: u64,
}

enum CapSide {
    Oldest,
    Newest,
}

/// One transcript row group. `id` is the item seq from the daemon for daemon
/// rows, and a client-allocated id above `CLIENT_ID_TAG` for rows the client
/// invents itself (notices, gaps, finished background jobs, a permission row
/// that arrived as an event rather than as an item).
#[derive(Clone, Debug)]
pub struct Cell {
    pub id: u64,
    pub kind: CellKind,
    /// Bumped whenever the content changes (invalidates the cache).
    pub version: u32,
    pub ts_ms: u64,
    pub stats: Option<String>,
    /// `e`-key explanation state — `Some` only while the cell still carries a
    /// pending `TranscriptKind::Permission` request; a verdict's swap to a
    /// notice drops it with the prompt.
    pub perm_explain: Option<PermExplain>,
}

/// Display state of the detached permission explanation. `e` on a pending
/// permission sends `Request::ExplainPermission`; the aux model's answer
/// arrives later as `Event::PermissionExplained`. It lives on the cell rather
/// than inside `TranscriptKind::Permission` — that is the wire item and has
/// no room for client-side state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermExplain {
    /// `e` was pressed; the daemon has not answered yet.
    Requested,
    /// The explanation landed. `level` is the model's danger rating —
    /// `None` when the aux call failed and `text` holds the honest reason.
    Done {
        text: String,
        level: Option<ExplainLevel>,
    },
}

/// The word shown for an explanation's danger rating — the same spellings
/// the aux model's `LEVEL:` line uses (`ExplainLevel::parse` accepts them).
pub fn explain_level_name(level: ExplainLevel) -> &'static str {
    match level {
        ExplainLevel::None => "none",
        ExplainLevel::Low => "low",
        ExplainLevel::Medium => "medium",
        ExplainLevel::High => "high",
        ExplainLevel::Catastrophic => "catastrophic",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HeaderInfo {
    pub version: String,
    /// Days since the Unix epoch; picks the phrase of the day (stable for the whole day).
    pub day: u64,
    pub mascot: crate::ui::mascot::MascotKind,
    /// Whether the terminal takes RGB, so the mascot can use the arcade palette
    /// instead of folding it down to the nearest ANSI colour.
    pub truecolor: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CellKind {
    /// Wraps the daemon item; `Assistant`/`Reasoning` text is the *revealed* text.
    Item(TranscriptKind),
    /// Divider between turns (drawn as a blank line).
    Gap,
    /// A background task or subagent that ended. Client-side only: the daemon
    /// reports the task, the client decides whether it was long enough to be
    /// worth a line and renders it like a tool call rather than as prose.
    BackgroundResult(BackgroundResult),
}

/// What a finished background job is shown as.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundResult {
    /// `task` for a command, `agent` for a subagent.
    pub kind: BackgroundKind,
    /// Command line or agent name.
    pub label: String,
    pub outcome: BackgroundOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundKind {
    Task,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundOutcome {
    Completed,
    Failed,
    Killed,
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

    pub fn set_max_cells(&mut self, max_cells: usize) {
        self.max_cells = max_cells;
        self.enforce_limits();
    }

    pub fn set_max_bytes(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
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

    fn is_evicted(&self, cell_id: u64) -> bool {
        !is_client_id(cell_id)
            && self.evicted_floor.is_some_and(|floor| cell_id <= floor)
            && self.cell_index(cell_id).is_none()
    }

    fn rendered_line_count(&self, cell_id: u64) -> usize {
        self.cell_line_index
            .get(&cell_id)
            .and_then(|index| self.cell_lines.get(*index))
            .filter(|(id, _, _)| *id == cell_id)
            .map(|(_, start, end)| end.saturating_sub(*start))
            .unwrap_or(0)
    }

    /// Whether the cell must survive an eviction pass: the streaming cell and
    /// any prompt still waiting for a verdict.
    fn is_pinned(cell_id: u64, live_cell: Option<u64>, kind: &CellKind) -> bool {
        Some(cell_id) == live_cell || matches!(kind, CellKind::Item(TranscriptKind::Permission(_)))
    }

    fn forget_evicted(&mut self, cell_id: u64) -> usize {
        self.expanded.remove(&cell_id);
        self.stable_revisions.remove(&cell_id);
        self.cache.invalidate_cell(cell_id);
        self.tool_streams.remove(cell_id);
        let size = self.forget_bytes(cell_id);
        if !is_client_id(cell_id) {
            self.evicted_floor = Some(
                self.evicted_floor
                    .map_or(cell_id, |floor| floor.max(cell_id)),
            );
        }
        size
    }

    fn replace_with_capped(
        &mut self,
        source: VecDeque<Cell>,
        side: CapSide,
        prefix_len: usize,
    ) -> (usize, usize) {
        let live_present = self
            .live_cell
            .is_some_and(|live_id| source.iter().any(|cell| cell.id == live_id));
        let limit = match side {
            CapSide::Oldest => self.max_cells,
            CapSide::Newest if live_present => self.max_cells.saturating_add(1),
            CapSide::Newest => self.max_cells,
        };
        if source.len() <= limit && self.bytes <= self.max_bytes {
            let retained_prefix = prefix_len.min(source.len());
            self.cells = source;
            return (0, retained_prefix);
        }

        let source_len = source.len();
        let mut to_evict = source_len.saturating_sub(limit);
        let mut to_free = self.bytes.saturating_sub(self.max_bytes);
        let mut retained = VecDeque::with_capacity(source_len.min(limit.max(1)));
        let mut retained_prefix = 0usize;
        let mut evicted_lines = 0usize;
        let live_cell = self.live_cell;

        match side {
            CapSide::Newest => {
                for (index, cell) in source.into_iter().enumerate().rev() {
                    let over = to_evict > 0 || to_free > 0;
                    if over && index > 0 && !Self::is_pinned(cell.id, live_cell, &cell.kind) {
                        evicted_lines =
                            evicted_lines.saturating_add(self.rendered_line_count(cell.id));
                        to_free = to_free.saturating_sub(self.forget_evicted(cell.id));
                        to_evict = to_evict.saturating_sub(1);
                    } else {
                        if index < prefix_len {
                            retained_prefix += 1;
                        }
                        retained.push_front(cell);
                    }
                }
            }
            CapSide::Oldest => {
                for (index, cell) in source.into_iter().enumerate() {
                    let over = to_evict > 0 || to_free > 0;
                    if over
                        && index + 1 < source_len
                        && !Self::is_pinned(cell.id, live_cell, &cell.kind)
                    {
                        evicted_lines =
                            evicted_lines.saturating_add(self.rendered_line_count(cell.id));
                        to_free = to_free.saturating_sub(self.forget_evicted(cell.id));
                        to_evict = to_evict.saturating_sub(1);
                    } else {
                        retained.push_back(cell);
                    }
                }
            }
        }

        self.cells = retained;
        (evicted_lines, retained_prefix)
    }

    /// Prepend older items (history page).
    pub fn prepend(&mut self, items: Vec<TranscriptItem>, has_more: bool) -> usize {
        self.has_more_history = has_more;
        self.loading_history = false;

        let mut seen: HashSet<u64> = self.cells.iter().map(|cell| cell.id).collect();
        let mut asked: HashSet<PermissionId> = self
            .cells
            .iter()
            .filter_map(|cell| match &cell.kind {
                CellKind::Item(TranscriptKind::Permission(req)) => Some(req.id.clone()),
                _ => None,
            })
            .collect();
        let mut older = VecDeque::with_capacity(items.len());
        for item in items {
            if let TranscriptKind::Permission(req) = &item.kind
                && !asked.insert(req.id.clone())
            {
                continue;
            }
            if !seen.insert(item.seq) {
                continue;
            }
            let cell = Cell {
                id: item.seq,
                kind: CellKind::Item(item.kind),
                version: 0,
                ts_ms: item.ts_ms,
                stats: None,
                perm_explain: None,
            };
            let size = cell_bytes(&cell);
            self.set_cell_bytes(cell.id, size);
            older.push_back(cell);
        }
        if older.is_empty() {
            return 0;
        }

        let older_len = older.len();
        let current = std::mem::take(&mut self.cells);
        let mut combined = VecDeque::with_capacity(older_len + current.len());
        for cell in older {
            combined.push_back(cell);
        }
        for cell in current {
            combined.push_back(cell);
        }

        let (evicted_lines, retained_prefix) =
            self.replace_with_capped(combined, CapSide::Newest, older_len);
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(evicted_lines);

        let Transcript {
            cells,
            expanded,
            cache,
            render_profile,
            tool_streams,
            ..
        } = self;
        render_profile.as_ref().map_or(0, |profile| {
            crate::ui::dialog::measure_cell_lines(
                cells.iter().take(retained_prefix),
                expanded,
                cache,
                tool_streams,
                &profile.options,
                profile.width,
                profile.anim_frame,
            )
        })
    }

    fn stable_prefix_len(cell: &Cell) -> Option<usize> {
        match &cell.kind {
            CellKind::Item(TranscriptKind::Assistant { text, .. }) => {
                Some(pacode_render::markdown::split_stable_tail(text).0.len())
            }
            _ => None,
        }
    }

    fn refresh_stable_revision(&mut self, cell_id: u64, current_len: Option<usize>, force: bool) {
        let Some(current_len) = current_len else {
            self.stable_revisions.remove(&cell_id);
            return;
        };
        let (mut revision, previous_len) = self
            .stable_revisions
            .remove(&cell_id)
            .unwrap_or((0, current_len));
        if force || previous_len != current_len {
            revision = revision.wrapping_add(1);
        }
        self.stable_revisions
            .insert(cell_id, (revision, current_len));
    }

    /// Set or update the non-persisted header banner.
    pub fn set_header(&mut self, info: HeaderInfo) {
        if self.header.as_ref() == Some(&info) {
            return;
        }
        self.header = Some(info);
        self.header_version = self.header_version.wrapping_add(1);
    }

    /// Hand the paced stream over to a different cell.
    ///
    /// The buffer belongs to whichever cell is live. Carrying it across a switch
    /// replayed one cell's undrained tail into the next one, which is how the
    /// first grapheme of an answer ended up alone in the reasoning cell above it
    /// while the rest of the sentence went to the answer cell. Anything still
    /// buffered is flushed into the cell that produced it before the new cell
    /// starts with an empty buffer.
    fn set_live_cell(&mut self, seq: u64, now: Instant) {
        if self.live_cell == Some(seq) {
            self.stream.get_or_insert_with(|| StreamBuffer::new(now));
            return;
        }
        if self.live_cell.is_some() && self.has_backlog() {
            self.flush_stream();
        }
        self.live_cell = Some(seq);
        self.stream = Some(StreamBuffer::new(now));
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

    /// `TextDelta`/`ReasoningDelta` for `item_seq`: queue into the stream buffer.
    pub fn push_delta(&mut self, item_seq: u64, text: &str, reasoning: bool, now: Instant) {
        if text.is_empty() {
            return;
        }
        let kind = if reasoning {
            StreamKind::Reasoning
        } else {
            StreamKind::Text
        };
        if self.live_cell == Some(item_seq) {
            let sb = self.stream.get_or_insert_with(|| StreamBuffer::new(now));
            sb.push(kind, text);
        } else if self.cell_index(item_seq).is_some() {
            self.set_live_cell(item_seq, now);
            if let Some(sb) = self.stream.as_mut() {
                sb.push(kind, text);
            }
            if let Some(index) = self.cell_index(item_seq) {
                let cell = &mut self.cells[index];
                cell.version = cell.version.wrapping_add(1);
            }
        }
    }

    /// Output a running tool call has produced, for the cell at `cell_id`.
    ///
    /// Anything that is not a tool cell still `Running` is refused, and clears
    /// the cell's buffer if it had one: a delta that outlives its call (a
    /// chunk in flight when the final item lands) must not leave a live body
    /// behind that nothing ever draws and nothing ever frees.
    pub fn push_tool_output(&mut self, cell_id: u64, chunk: &str) {
        let running = self.cell_index(cell_id).is_some_and(|index| {
            matches!(
                &self.cells[index].kind,
                CellKind::Item(TranscriptKind::ToolCall {
                    status: ToolStatus::Running,
                    ..
                })
            )
        });

        if running {
            self.tool_streams.push(cell_id, chunk);
        } else {
            self.tool_streams.remove(cell_id);
        }
    }

    /// Reveal into the live cell: the stable prefix length, and the bytes the
    /// cell grew by.
    fn apply_stream_ops(&mut self, index: usize, ops: Vec<StreamOp>) -> Option<(usize, usize)> {
        let cell = self.cells.get_mut(index)?;
        let mut added = 0usize;
        for op in ops {
            match op {
                StreamOp::Text(text) => {
                    if let CellKind::Item(TranscriptKind::Assistant {
                        text: cell_text, ..
                    }) = &mut cell.kind
                    {
                        cell_text.push_str(&text);
                        added = added.saturating_add(text.len());
                    }
                }
                StreamOp::Reasoning(text) => {
                    if let CellKind::Item(TranscriptKind::Reasoning {
                        text: cell_text, ..
                    }) = &mut cell.kind
                    {
                        cell_text.push_str(&text);
                        added = added.saturating_add(text.len());
                    }
                }
                StreamOp::CloseReasoning => {}
            }
        }
        cell.version = cell.version.wrapping_add(1);
        Self::stable_prefix_len(cell).map(|stable_len| (stable_len, added))
    }

    fn grow_cell_bytes(&mut self, cell_id: u64, added: usize) {
        if added == 0 {
            return;
        }
        let size = self.cell_bytes.entry(cell_id).or_default();
        *size = size.saturating_add(added);
        self.bytes = self.bytes.saturating_add(added);
    }

    /// Reveal paced text into the live cell. Returns true when something was revealed.
    pub fn tick_stream(&mut self, now: Instant) -> bool {
        let Some(live_id) = self.live_cell else {
            return false;
        };
        let Some(ref mut sb) = self.stream else {
            return false;
        };
        let ops = sb.reveal(now);
        if ops.is_empty() {
            return false;
        }

        let Some(index) = self.cell_index(live_id) else {
            return false;
        };
        let Some((stable_len, added)) = self.apply_stream_ops(index, ops) else {
            return false;
        };
        self.grow_cell_bytes(live_id, added);
        self.refresh_stable_revision(live_id, Some(stable_len), false);
        self.apply_pending_final();
        true
    }

    /// When the backlog is drained and a final item is waiting, install it.
    fn apply_pending_final(&mut self) {
        if self.has_backlog() {
            return;
        }
        if let Some(item) = self.pending_final.take() {
            self.live_cell = None;
            self.stream = None;
            self.insert_or_replace(item.seq, item.kind, item.ts_ms);
        }
    }

    /// Flush the stream (item complete or interrupted).
    pub fn flush_stream(&mut self) {
        let Some(live_id) = self.live_cell else {
            return;
        };
        let Some(ref mut sb) = self.stream else {
            return;
        };
        let ops = sb.flush();
        if let Some(index) = self.cell_index(live_id)
            && let Some((stable_len, added)) = self.apply_stream_ops(index, ops)
        {
            self.grow_cell_bytes(live_id, added);
            self.refresh_stable_revision(live_id, Some(stable_len), false);
        }
        if let Some(item) = self.pending_final.take() {
            self.live_cell = None;
            self.stream = None;
            self.insert_or_replace(item.seq, item.kind, item.ts_ms);
        }
    }

    pub fn has_backlog(&self) -> bool {
        self.stream.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn stream_backlog_chars(&self) -> usize {
        self.stream.as_ref().map(|s| s.backlog_chars()).unwrap_or(0)
    }

    pub fn has_pending_final(&self) -> bool {
        self.pending_final.is_some()
    }

    /// Record the rendered line count and viewport height from the most recent draw,
    /// clamping `scroll_from_bottom` so the view can never sit past the top.
    /// The cell drawn at content line `line`, from the most recent draw.
    pub fn cell_at_line(&self, line: usize) -> Option<u64> {
        let index = self.cell_lines.partition_point(|(_, _, end)| *end <= line);
        let (cell_id, start, end) = self.cell_lines.get(index)?;
        (*start <= line && line < *end).then_some(*cell_id)
    }

    /// `ctrl+o`: expand the newest tool cell, or collapse it back when it is
    /// already expanded. Every tool call is expandable — any status (a denied
    /// call reveals the refused command) and any preview length (expansion
    /// also shows the call's full argument, which the collapsed cell never
    /// had room for). A `!cmd` result is drawn as a bash call and folds the
    /// same way, so it expands too. Returns true on change.
    pub fn toggle_last_expandable(&mut self) -> bool {
        let id = self.cells.iter().rev().find_map(|c| match &c.kind {
            CellKind::Item(
                TranscriptKind::ToolCall { .. } | TranscriptKind::BashCommand { .. },
            ) => Some(c.id),
            _ => None,
        });
        let Some(id) = id else { return false };
        self.toggle_expanded(id)
    }

    /// Show or hide a cell's full output. Returns true when something changed.
    pub fn toggle_expanded(&mut self, cell_id: u64) -> bool {
        if self.cell_index(cell_id).is_none() {
            return false;
        }
        if !self.expanded.remove(&cell_id) {
            self.expanded.insert(cell_id);
        }
        // The rendered height changes, so the cached lines for that cell are stale.
        self.cache.invalidate_cell(cell_id);
        true
    }

    pub fn is_expanded(&self, cell_id: u64) -> bool {
        self.expanded.contains(&cell_id)
    }

    pub fn record_render(&mut self, total_lines: usize, viewport: usize) {
        self.rendered_lines = Some(total_lines);
        self.viewport_height = Some(viewport);
        let max_scroll = total_lines.saturating_sub(viewport);
        self.scroll_from_bottom = self.scroll_from_bottom.min(max_scroll);
        self.first_visible_line = max_scroll.saturating_sub(self.scroll_from_bottom);
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let max_scroll = match (self.rendered_lines, self.viewport_height) {
            (Some(total), Some(vp)) => total.saturating_sub(vp),
            _ => 0,
        };
        let new_scroll =
            (self.scroll_from_bottom as i64 + delta as i64).clamp(0, max_scroll as i64);
        self.scroll_from_bottom = new_scroll as usize;
        self.first_visible_line = max_scroll.saturating_sub(self.scroll_from_bottom);
    }

    pub fn scroll_to_top(&mut self) {
        let max_scroll = match (self.rendered_lines, self.viewport_height) {
            (Some(total), Some(vp)) => total.saturating_sub(vp),
            _ => 0,
        };
        self.scroll_from_bottom = max_scroll;
        self.first_visible_line = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_from_bottom = 0;
        self.first_visible_line = self
            .rendered_lines
            .zip(self.viewport_height)
            .map_or(0, |(total, viewport)| total.saturating_sub(viewport));
    }

    /// Oldest item seq still on screen: the cursor a history page is asked for.
    /// Client rows are skipped — a notice carries no seq, and handing its id to
    /// `GetHistory` would page from the start of the session forever.
    pub fn oldest_seq(&self) -> Option<u64> {
        self.cells
            .iter()
            .find(|cell| !is_client_id(cell.id))
            .map(|cell| cell.id)
    }

    pub fn is_at_top(&self) -> bool {
        match (self.rendered_lines, self.viewport_height) {
            (Some(total), Some(vp)) => {
                if total == 0 {
                    return false;
                }
                let max_scroll = total.saturating_sub(vp);
                self.scroll_from_bottom >= max_scroll
            }
            _ => false,
        }
    }

    /// Screen position (0-based line offset from the top of the viewport) for a cell.
    /// Returns `None` if the cell is not currently within the visible viewport.
    pub fn cell_screen_position(&mut self, cell_id: u64, viewport: usize) -> Option<usize> {
        if viewport == 0 {
            return None;
        }
        self.sync_cell_line_index();
        let index = *self.cell_line_index.get(&cell_id)?;
        let (id, start, end) = self.cell_lines.get(index)?;
        if *id != cell_id {
            return None;
        }
        let first = self.first_visible_line;
        let viewport_end = first.saturating_add(viewport);
        if *end <= first || *start >= viewport_end {
            None
        } else {
            Some(start.saturating_sub(first))
        }
    }

    /// `cell_lines` is rewritten by the render pass, which keeps the index in
    /// step; anything that clears the spans without the index (a reset) leaves
    /// a map pointing at rows that no longer exist, so rebuild on the length
    /// mismatch before trusting a position.
    fn sync_cell_line_index(&mut self) {
        if self.cell_line_index.len() == self.cell_lines.len() {
            return;
        }
        self.cell_line_index.clear();
        self.cell_line_index.extend(
            self.cell_lines
                .iter()
                .enumerate()
                .map(|(index, (id, _, _))| (*id, index)),
        );
    }

    /// Evict cells past `max_cells` or `max_bytes` — oldest first, never the
    /// streaming cell or a prompt waiting for a verdict — dropping the cache of
    /// everything that goes.
    fn enforce_limits(&mut self) {
        if self.cells.len() <= self.max_cells && self.bytes <= self.max_bytes {
            return;
        }
        let source = std::mem::take(&mut self.cells);
        let _ = self.replace_with_capped(source, CapSide::Oldest, 0);
    }
}
