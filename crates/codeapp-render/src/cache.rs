//! Bounded cache of rendered lines keyed by (cell id, width). Total cached lines are
//! capped; eviction is LRU by cell. The TUI owns one per transcript.

use ratatui::text::Line;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub cell: u64,
    pub width: u16,
}

pub struct LineCache {
    _private: (),
}

impl LineCache {
    /// `max_lines`: total lines kept across all entries.
    pub fn new(max_lines: usize) -> Self {
        let _ = max_lines;
        todo!("LineCache::new")
    }

    /// Borrow cached lines, marking the entry recently used.
    pub fn get(&mut self, key: CacheKey) -> Option<&[Line<'static>]> {
        let _ = key;
        todo!("LineCache::get")
    }

    /// Insert (replacing any entry for the same key) and evict LRU entries until under
    /// `max_lines`. The inserted entry itself is never evicted by its own insert.
    pub fn insert(&mut self, key: CacheKey, lines: Vec<Line<'static>>) -> &[Line<'static>] {
        let _ = (key, lines);
        todo!("LineCache::insert")
    }

    /// Drop every entry for `cell` (content changed).
    pub fn invalidate_cell(&mut self, cell: u64) {
        let _ = cell;
        todo!("LineCache::invalidate_cell")
    }

    /// Drop all entries (width change).
    pub fn clear(&mut self) {
        todo!("LineCache::clear")
    }

    pub fn total_lines(&self) -> usize {
        todo!("LineCache::total_lines")
    }
}
