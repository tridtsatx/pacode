//! Bounded cache of rendered lines keyed by (cell id, width). Total cached lines are
//! capped; eviction is LRU by cell. The TUI owns one per transcript.

use std::collections::{HashMap, VecDeque};

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
}

pub struct LineCache {
    entries: HashMap<CacheKey, Entry>,
    lru: VecDeque<CacheKey>,
    total_lines: usize,
    max_lines: usize,
}

impl LineCache {
    /// `max_lines`: total lines kept across all entries.
    pub fn new(max_lines: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            total_lines: 0,
            max_lines,
        }
    }

    /// Borrow cached lines, marking the entry recently used.
    pub fn get(&mut self, key: CacheKey) -> Option<&[Line<'static>]> {
        if !self.entries.contains_key(&key) {
            return None;
        }
        if let Some(pos) = self.lru.iter().position(|k| *k == key) {
            self.lru.remove(pos);
        }
        self.lru.push_back(key);
        self.entries.get(&key).map(|e| e.lines.as_slice())
    }

    /// Insert (replacing any entry for the same key) and evict LRU entries until under
    /// `max_lines`. The inserted entry itself is never evicted by its own insert.
    pub fn insert(&mut self, key: CacheKey, lines: Vec<Line<'static>>) -> &[Line<'static>] {
        let len = lines.len();

        if let Some(old) = self.entries.remove(&key) {
            self.total_lines = self.total_lines.saturating_sub(old.len);
            if let Some(pos) = self.lru.iter().position(|k| *k == key) {
                self.lru.remove(pos);
            }
        }

        self.total_lines += len;
        self.entries.insert(key, Entry { lines, len });
        self.lru.push_back(key);

        while self.total_lines > self.max_lines && self.lru.len() > 1 {
            if let Some(front_key) = self.lru.pop_front() {
                if front_key == key {
                    self.lru.push_front(front_key);
                    break;
                }
                if let Some(evicted) = self.entries.remove(&front_key) {
                    self.total_lines = self.total_lines.saturating_sub(evicted.len);
                }
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
            }
        }

        self.lru.retain(|k| k.cell != cell);
    }

    /// Drop all entries (width change).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.total_lines = 0;
    }

    pub fn total_lines(&self) -> usize {
        self.total_lines
    }
}
