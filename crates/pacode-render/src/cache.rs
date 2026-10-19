//! Bounded cache of rendered lines keyed by (cell id, width). Total cached lines are
//! capped; eviction is LRU by cell. The TUI owns one per transcript.
//!
//! Recency is a monotonically increasing `stamp` per entry plus a `BTreeMap<stamp, key>`
//! ordered oldest first, so `get`, `insert` and eviction are O(log n) in the number of
//! entries (the old `VecDeque` scan was O(n) on every hit).

use std::collections::{BTreeMap, HashMap};

use ratatui::text::Line;

#[cfg(test)]
#[path = "cache_tests.rs"]
mod cache_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub cell: u64,
    pub width: u16,
}

struct Entry {
    lines: Vec<Line<'static>>,
    len: usize,
    /// Position in `LineCache::order`; refreshed on every hit.
    stamp: u64,
}

/// Cap: `max_lines` rendered lines in total (the entry just inserted always stays).
/// Invalidation: `invalidate_cell` (one cell), `clear` (everything; also bumps
/// `generation`). Owner: the transcript view state.
pub struct LineCache {
    entries: HashMap<CacheKey, Entry>,
    /// Oldest first: `stamp -> key`. Holds exactly one item per entry.
    order: BTreeMap<u64, CacheKey>,
    next_stamp: u64,
    /// Bumped by `clear`, so derived data (cached line counts) can tell the cache was reset.
    generation: u64,
    total_lines: usize,
    max_lines: usize,
}

impl LineCache {
    /// `max_lines`: total lines kept across all entries.
    pub fn new(max_lines: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            next_stamp: 0,
            generation: 0,
            total_lines: 0,
            max_lines,
        }
    }

    fn fresh_stamp(&mut self) -> u64 {
        let stamp = self.next_stamp;
        self.next_stamp = self.next_stamp.wrapping_add(1);
        stamp
    }

    /// Borrow cached lines, marking the entry recently used.
    pub fn get(&mut self, key: CacheKey) -> Option<&[Line<'static>]> {
        let old = self.entries.get(&key)?.stamp;
        let stamp = self.fresh_stamp();
        self.order.remove(&old);
        self.order.insert(stamp, key);
        let entry = self.entries.get_mut(&key)?;
        entry.stamp = stamp;
        Some(entry.lines.as_slice())
    }

    /// True when `key` is cached. Does not touch recency.
    pub fn contains(&self, key: CacheKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// Insert (replacing any entry for the same key) and evict LRU entries until under
    /// `max_lines`. The inserted entry itself is never evicted by its own insert.
    pub fn insert(&mut self, key: CacheKey, lines: Vec<Line<'static>>) -> &[Line<'static>] {
        let len = lines.len();

        if let Some(old) = self.entries.remove(&key) {
            self.total_lines = self.total_lines.saturating_sub(old.len);
            self.order.remove(&old.stamp);
        }

        let stamp = self.fresh_stamp();
        self.total_lines += len;
        self.entries.insert(key, Entry { lines, len, stamp });
        self.order.insert(stamp, key);

        while self.total_lines > self.max_lines && self.order.len() > 1 {
            let Some((oldest_stamp, oldest_key)) = self.order.pop_first() else {
                break;
            };
            if oldest_key == key {
                // Only reachable if the new entry is the oldest; keep it.
                self.order.insert(oldest_stamp, oldest_key);
                break;
            }
            if let Some(evicted) = self.entries.remove(&oldest_key) {
                self.total_lines = self.total_lines.saturating_sub(evicted.len);
            }
        }

        self.entries
            .get(&key)
            .map(|e| e.lines.as_slice())
            .unwrap_or(&[])
    }

    /// Drop every entry for `cell` (content changed).
    pub fn invalidate_cell(&mut self, cell: u64) {
        let keys_to_remove: Vec<CacheKey> = self
            .entries
            .keys()
            .filter(|k| k.cell == cell)
            .copied()
            .collect();

        for key in keys_to_remove {
            if let Some(removed) = self.entries.remove(&key) {
                self.total_lines = self.total_lines.saturating_sub(removed.len);
                self.order.remove(&removed.stamp);
            }
        }
    }

    /// Drop all entries (width change).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.generation = self.generation.wrapping_add(1);
        self.total_lines = 0;
    }

    /// Changes whenever `clear` runs.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn total_lines(&self) -> usize {
        self.total_lines
    }
}
