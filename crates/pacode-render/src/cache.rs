//! Bounded cache of rendered lines keyed by (cell id, width). Total cached lines are
//! capped; eviction is LRU by cell. The TUI owns one per transcript.

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
    /// Stamp in `order`: recency is a sequence number, not a list position, so
    /// a touch is one BTreeMap remove + insert instead of a linear scan.
    seq: u64,
}

pub struct LineCache {
    entries: HashMap<CacheKey, Entry>,
    /// `seq -> key`, oldest at the front; the LRU order of `entries`.
    order: BTreeMap<u64, CacheKey>,
    /// Next recency stamp. Monotone; wrap is unreachable in practice.
    next_seq: u64,
    total_lines: usize,
    max_lines: usize,
}

impl LineCache {
    /// `max_lines`: total lines kept across all entries.
    pub fn new(max_lines: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            next_seq: 0,
            total_lines: 0,
            max_lines,
        }
    }

    /// Borrow cached lines, marking the entry recently used.
    pub fn get(&mut self, key: CacheKey) -> Option<&[Line<'static>]> {
        let entry = self.entries.get_mut(&key)?;
        self.order.remove(&entry.seq);
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        entry.seq = seq;
        self.order.insert(seq, key);
        Some(entry.lines.as_slice())
    }

    /// Borrow cached lines without touching recency — the read-only counterpart
    /// of `get` for callers that already know the entry is warm (the render
    /// pass touches recency in its `&mut` phase, then borrows through `&`).
    pub fn peek(&self, key: CacheKey) -> Option<&[Line<'static>]> {
        self.entries.get(&key).map(|e| e.lines.as_slice())
    }

    /// Insert (replacing any entry for the same key) and evict LRU entries until under
    /// `max_lines`. The inserted entry itself is never evicted by its own insert.
    pub fn insert(&mut self, key: CacheKey, lines: Vec<Line<'static>>) -> &[Line<'static>] {
        let len = lines.len();

        if let Some(old) = self.entries.remove(&key) {
            self.total_lines = self.total_lines.saturating_sub(old.len);
            self.order.remove(&old.seq);
        }

        self.total_lines += len;
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.entries.insert(key, Entry { lines, len, seq });
        self.order.insert(seq, key);

        while self.total_lines > self.max_lines && self.order.len() > 1 {
            if let Some((_, front_key)) = self.order.pop_first() {
                if front_key == key {
                    self.order.insert(seq, key);
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
                self.order.remove(&removed.seq);
            }
        }
    }

    /// Drop all entries (width change).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.total_lines = 0;
    }

    pub fn total_lines(&self) -> usize {
        self.total_lines
    }
}
