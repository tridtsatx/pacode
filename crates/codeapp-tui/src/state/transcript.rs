//! Transcript cells with a bounded render cache and paced streaming.

use std::collections::VecDeque;
use std::time::Instant;

use codeapp_render::{LineCache, StreamBuffer};
use codeapp_types::{TranscriptItem, TranscriptKind};

/// One transcript row group. `id` is the item seq from the daemon (stable across
/// updates), `cache_id` keys the render cache.
pub struct Cell {
    pub id: u64,
    pub kind: CellKind,
    /// Bumped whenever the content changes (invalidates the cache).
    pub version: u32,
    pub ts_ms: u64,
}

pub enum CellKind {
    /// Wraps the daemon item; `Assistant`/`Reasoning` text is the *revealed* text.
    Item(TranscriptKind),
    /// Divider between turns (drawn as a blank line).
    Gap,
}

pub struct Transcript {
    pub cells: VecDeque<Cell>,
    pub max_cells: usize,
    /// Scroll offset in rendered lines from the bottom; 0 = follow the tail.
    pub scroll_from_bottom: usize,
    pub cache: LineCache,
    /// Streaming reveal buffer for the live assistant cell (`live_cell`).
    pub stream: Option<StreamBuffer>,
    pub live_cell: Option<u64>,
    /// Hidden text that arrived but is not revealed yet is inside `stream`.
    pub has_more_history: bool,
    /// Set while a `GetHistory` request is in flight.
    pub loading_history: bool,
}

impl Transcript {
    pub fn new(max_cells: usize) -> Self {
        Self {
            cells: VecDeque::new(),
            max_cells,
            scroll_from_bottom: 0,
            cache: LineCache::new(20_000),
            stream: None,
            live_cell: None,
            has_more_history: false,
            loading_history: false,
        }
    }

    /// Replace everything (snapshot).
    pub fn reset(&mut self, items: Vec<TranscriptItem>, has_more: bool) {
        let _ = (items, has_more);
        todo!("Transcript::reset")
    }

    /// Prepend older items (history page).
    pub fn prepend(&mut self, items: Vec<TranscriptItem>, has_more: bool) {
        let _ = (items, has_more);
        todo!("Transcript::prepend")
    }

    /// `ItemAdded` / `ItemUpdated`: insert or replace by seq. Assistant/Reasoning items
    /// that are not complete become the live cell with an empty revealed text.
    pub fn upsert(&mut self, item: TranscriptItem, now: Instant) {
        let _ = (item, now);
        todo!("Transcript::upsert")
    }

    /// `TextDelta`/`ReasoningDelta` for `item_seq`: queue into the stream buffer.
    pub fn push_delta(&mut self, item_seq: u64, text: &str, reasoning: bool, now: Instant) {
        let _ = (item_seq, text, reasoning, now);
        todo!("Transcript::push_delta")
    }

    /// Reveal paced text into the live cell. Returns true when something was revealed.
    pub fn tick_stream(&mut self, now: Instant) -> bool {
        let _ = now;
        todo!("Transcript::tick_stream")
    }

    /// Flush the stream (item complete or interrupted).
    pub fn flush_stream(&mut self) {
        todo!("Transcript::flush_stream")
    }

    pub fn has_backlog(&self) -> bool {
        self.stream.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn scroll_by(&mut self, delta: i32, total_lines: usize, viewport: usize) {
        let _ = (delta, total_lines, viewport);
        todo!("Transcript::scroll_by")
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_from_bottom = 0;
    }

    /// Evict oldest cells past `max_cells` (keeping the live cell), dropping their cache.
    pub fn enforce_cap(&mut self) {
        todo!("Transcript::enforce_cap")
    }
}
