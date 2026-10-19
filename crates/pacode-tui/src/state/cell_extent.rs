//! Per-cell rendered line counts, so a frame can lay out the whole transcript
//! (scroll position, click map, "N new lines") without rendering every cell.
//!
//! Cap: one record per cell currently in the transcript (`max_cells`, 500 by default);
//! a record is 48 bytes. Invalidation: a record only answers for the exact
//! (version, width, expanded, thinking, has_stats, live, context, group) it was measured
//! with; eviction drops evicted cells, and a reset of the line cache (its `generation`
//! changes) drops everything. Owner: `Transcript`. Nothing here ticks; it is only read
//! and written while drawing.
//!
//! A run of exploration calls drawn as one group is recorded under its first cell's
//! id with `group` set to the group's fingerprint (`toolgroup::cache_key`), so a cell
//! that is drawn alone in one frame and heads a group in the next never answers for
//! the other shape.

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
    /// The render context (`RenderOptions::cache_epoch`): theme, glyphs, hints.
    pub context: u64,
    /// The group fingerprint when the record is a group's block; 0 for one cell.
    pub group: u64,
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

    /// Keep only the records `keep` answers true for (by cell id).
    pub fn retain(&mut self, mut keep: impl FnMut(u64) -> bool) {
        self.map.retain(|id, _| keep(*id));
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
