//! Transcript cells with a bounded render cache and paced streaming.

use std::collections::VecDeque;
use std::time::Instant;

use pacode_render::{LineCache, StreamBuffer, StreamKind, StreamOp};
use pacode_types::{TranscriptItem, TranscriptKind};

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod transcript_tests;

/// One transcript row group. `id` is the item seq from the daemon (stable across
/// updates), `cache_id` keys the render cache.
#[derive(Clone, Debug)]
pub struct Cell {
    pub id: u64,
    pub kind: CellKind,
    /// Bumped whenever the content changes (invalidates the cache).
    pub version: u32,
    pub ts_ms: u64,
    pub stats: Option<String>,
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
    pub expanded: std::collections::HashSet<u64>,
    /// Which content lines belong to which cell, from the most recent draw:
    /// `(cell id, first line, one past the last)`. This is how a click on a row
    /// finds the cell under it.
    pub cell_lines: Vec<(u64, usize, usize)>,
    pub cache: LineCache,
    /// Streaming reveal buffer for the live assistant cell (`live_cell`).
    pub stream: Option<StreamBuffer>,
    pub live_cell: Option<u64>,
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
            scroll_from_bottom: 0,
            rendered_lines: None,
            viewport_height: None,
            first_visible_line: 0,
            expanded: std::collections::HashSet::new(),
            cell_lines: Vec::new(),
            cache: LineCache::new(20_000),
            stream: None,
            live_cell: None,
            pending_final: None,
            has_more_history: false,
            loading_history: false,
        }
    }

    /// Replace everything (snapshot).
    pub fn reset(&mut self, items: Vec<TranscriptItem>, has_more: bool) {
        self.cells.clear();
        self.cache.clear();
        self.stream = None;
        self.live_cell = None;
        self.pending_final = None;
        self.scroll_from_bottom = 0;
        self.loading_history = false;
        self.rendered_lines = None;
        self.viewport_height = None;

        // The daemon already bounds what it sends to `session.history_page` items and
        // reports whether anything older exists, so the client keeps the page whole
        // instead of capping it a second time with a number of its own.
        self.has_more_history = has_more;

        for item in &items {
            let cell = Cell {
                id: item.seq,
                kind: CellKind::Item(item.kind.clone()),
                version: 0,
                ts_ms: item.ts_ms,
                stats: None,
            };
            self.cells.push_back(cell);
        }
        self.enforce_cap();
    }

    /// Prepend older items (history page).
    pub fn prepend(&mut self, items: Vec<TranscriptItem>, has_more: bool) {
        self.has_more_history = has_more;
        self.loading_history = false;

        for item in items.into_iter().rev() {
            let cell = Cell {
                id: item.seq,
                kind: CellKind::Item(item.kind),
                version: 0,
                ts_ms: item.ts_ms,
                stats: None,
            };
            self.cells.push_front(cell);
        }
        self.enforce_cap();
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
                self.insert_or_replace(seq, other, item.ts_ms);
            }
        }
        self.enforce_cap();
    }

    fn insert_or_replace(&mut self, seq: u64, kind: TranscriptKind, ts_ms: u64) {
        if let Some(existing) = self.cells.iter_mut().find(|c| c.id == seq) {
            existing.kind = CellKind::Item(kind);
            existing.version = existing.version.wrapping_add(1);
            existing.ts_ms = ts_ms;
        } else {
            self.cells.push_back(Cell {
                id: seq,
                kind: CellKind::Item(kind),
                version: 0,
                ts_ms,
                stats: None,
            });
        }
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
        } else if self.cells.iter().any(|c| c.id == item_seq) {
            self.set_live_cell(item_seq, now);
            if let Some(sb) = self.stream.as_mut() {
                sb.push(kind, text);
            }
            if let Some(cell) = self.cells.iter_mut().find(|c| c.id == item_seq) {
                cell.version = cell.version.wrapping_add(1);
            }
        }
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

        if let Some(cell) = self.cells.iter_mut().find(|c| c.id == live_id) {
            for op in ops {
                match op {
                    // Text only ever reaches the cell that produced it now that
                    // the buffer is per-cell; a reasoning cell is never rewritten
                    // into an answer cell mid-stream.
                    StreamOp::Text(s) => {
                        if let CellKind::Item(TranscriptKind::Assistant { text, .. }) =
                            &mut cell.kind
                        {
                            text.push_str(&s);
                        }
                    }
                    StreamOp::Reasoning(s) => {
                        if let CellKind::Item(TranscriptKind::Reasoning { text, .. }) =
                            &mut cell.kind
                        {
                            text.push_str(&s);
                        }
                    }
                    StreamOp::CloseReasoning => {}
                }
            }
            cell.version = cell.version.wrapping_add(1);
            self.apply_pending_final();
            true
        } else {
            false
        }
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
        if let Some(cell) = self.cells.iter_mut().find(|c| c.id == live_id) {
            for op in ops {
                match op {
                    // Text only ever reaches the cell that produced it now that
                    // the buffer is per-cell; a reasoning cell is never rewritten
                    // into an answer cell mid-stream.
                    StreamOp::Text(s) => {
                        if let CellKind::Item(TranscriptKind::Assistant { text, .. }) =
                            &mut cell.kind
                        {
                            text.push_str(&s);
                        }
                    }
                    StreamOp::Reasoning(s) => {
                        if let CellKind::Item(TranscriptKind::Reasoning { text, .. }) =
                            &mut cell.kind
                        {
                            text.push_str(&s);
                        }
                    }
                    StreamOp::CloseReasoning => {}
                }
            }
            cell.version = cell.version.wrapping_add(1);
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
        self.cell_lines
            .iter()
            .find(|(_, start, end)| line >= *start && line < *end)
            .map(|(id, _, _)| *id)
    }

    /// Show or hide a cell's full output. Returns true when something changed.
    pub fn toggle_expanded(&mut self, cell_id: u64) -> bool {
        if !self.cells.iter().any(|c| c.id == cell_id) {
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
    }

    pub fn scroll_to_top(&mut self) {
        let max_scroll = match (self.rendered_lines, self.viewport_height) {
            (Some(total), Some(vp)) => total.saturating_sub(vp),
            _ => 0,
        };
        self.scroll_from_bottom = max_scroll;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_from_bottom = 0;
    }

    pub fn oldest_seq(&self) -> Option<u64> {
        self.cells.front().map(|c| c.id)
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
    pub fn cell_screen_position(&self, cell_id: u64, viewport: usize) -> Option<usize> {
        let cell_idx = self.cells.iter().position(|c| c.id == cell_id)?;
        let total = self.cells.len();
        if total == 0 || viewport == 0 {
            return None;
        }
        let max_scroll = total.saturating_sub(viewport);
        let scroll = self.scroll_from_bottom.min(max_scroll);
        let start = max_scroll.saturating_sub(scroll);
        if cell_idx >= start && cell_idx < start + viewport {
            Some(cell_idx - start)
        } else {
            None
        }
    }

    /// Evict oldest cells past `max_cells` (keeping the live cell and pinned header), dropping their cache.
    pub fn enforce_cap(&mut self) {
        let limit = self.max_cells;
        let evict_idx = 0;

        while self.cells.len() > limit {
            if let Some(cell) = self.cells.get(evict_idx) {
                if Some(cell.id) == self.live_cell {
                    break;
                }
                if let Some(evicted) = self.cells.remove(evict_idx) {
                    // The expansion belongs to the cell; an evicted cell takes it
                    // with it rather than leaving an id behind forever.
                    self.expanded.remove(&evicted.id);
                    self.cache.invalidate_cell(evicted.id);
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    }
}
