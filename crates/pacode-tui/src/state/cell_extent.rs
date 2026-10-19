//! Per-cell rendered line counts, so a frame can lay out the whole transcript
//! (scroll position, click map, "N new lines") without rendering every cell.
//!
//! Cap: one record per cell currently in the transcript (`max_cells`, 500 by default);
//! a record is 32 bytes. Invalidation: a record only answers for the exact
//! (version, width, expanded, thinking, has_stats, live) it was measured with; `enforce_cap`
//! drops evicted cells, and a reset of the line cache (its `generation` changes) drops
//! everything. Owner: `Transcript`. Nothing here ticks; it is only read and written
//! while drawing.

use std::collections::HashMap;

/// What a line count depends on besides the cell's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtentKey {
    pub version: u32,
    pub width: u16,
    pub expanded: bool,
    pub thinking: bool,
    pub has_stats: bool,
    /// The live (streaming) cell renders through a different path.
    pub live: bool,
}

#[derive(Debug, Default)]
pub struct CellExtents {
    map: HashMap<u64, (ExtentKey, usize)>,
    /// `LineCache::generation` the records were measured under.
    cache_generation: u64,
}

impl CellExtents {
    /// Drop every record when the line cache was cleared since they were taken.
    pub fn sync(&mut self, cache_generation: u64) {
        if self.cache_generation != cache_generation {
            self.map.clear();
            self.cache_generation = cache_generation;
        }
    }

    /// The measured line count for `id`, if it was measured under exactly `key`.
    pub fn get(&self, id: u64, key: &ExtentKey) -> Option<usize> {
        match self.map.get(&id) {
            Some((k, lines)) if k == key => Some(*lines),
            Some(_) | None => None,
        }
    }

    pub fn set(&mut self, id: u64, key: ExtentKey, lines: usize) {
        self.map.insert(id, (key, lines));
    }

    pub fn remove(&mut self, id: u64) {
        self.map.remove(&id);
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
