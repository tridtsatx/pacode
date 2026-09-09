//! Bounded head/tail output buffer. Port of codex `core/src/unified_exec/head_tail_buffer.rs`:
//! keeps the first `head_cap` bytes and the last `tail_cap` bytes, counts everything.

use std::collections::VecDeque;

pub struct HeadTailBuffer {
    head_cap: usize,
    tail_cap: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: u64,
}

impl HeadTailBuffer {
    pub fn new(head_cap: usize, tail_cap: usize) -> Self {
        Self {
            head_cap,
            tail_cap,
            head: Vec::with_capacity(head_cap.min(16 * 1024)),
            tail: VecDeque::with_capacity(tail_cap.min(64 * 1024)),
            total_bytes: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.total_bytes += bytes.len() as u64;

        let remaining = if self.head.len() < self.head_cap {
            let available = self.head_cap - self.head.len();
            let to_take = available.min(bytes.len());
            self.head.extend_from_slice(&bytes[..to_take]);
            &bytes[to_take..]
        } else {
            bytes
        };

        if self.tail_cap > 0 && !remaining.is_empty() {
            if remaining.len() >= self.tail_cap {
                self.tail.clear();
                self.tail
                    .extend(&remaining[remaining.len() - self.tail_cap..]);
            } else {
                let excess = (self.tail.len() + remaining.len()).saturating_sub(self.tail_cap);
                if excess > 0 {
                    self.tail.drain(..excess);
                }
                self.tail.extend(remaining);
            }
            self.tail.make_contiguous();
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn head(&self) -> &[u8] {
        &self.head
    }

    pub fn tail(&self) -> &[u8] {
        self.tail.as_slices().0
    }

    /// Lossy UTF-8 text: head, a `\n[... N bytes omitted ...]\n` marker when anything was
    /// dropped, tail. Then capped to `max_chars` (head+tail again).
    pub fn render(&self, max_chars: usize) -> String {
        let head_str = String::from_utf8_lossy(&self.head);
        let tail_str = String::from_utf8_lossy(self.tail());
        let cap = (self.head_cap + self.tail_cap) as u64;
        let text = if self.total_bytes > cap {
            let omitted = self.total_bytes - cap;
            format!("{head_str}\n[... {omitted} bytes omitted ...]\n{tail_str}")
        } else {
            format!("{head_str}{tail_str}")
        };
        codeapp_types::truncate_head_tail(&text, max_chars)
    }

    /// Last `n` complete lines of the tail.
    pub fn tail_lines(&self, n: usize) -> Vec<String> {
        if n == 0 {
            return Vec::new();
        }
        let cap = (self.head_cap + self.tail_cap) as u64;
        let text = if self.total_bytes > cap {
            String::from_utf8_lossy(self.tail())
        } else {
            let mut full = Vec::with_capacity(self.head.len() + self.tail.len());
            full.extend_from_slice(&self.head);
            full.extend(self.tail.iter().copied());
            String::from_utf8_lossy(&full).into_owned().into()
        };
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(n);
        lines[start..].iter().map(|s| s.to_string()).collect()
    }
}

#[cfg(test)]
#[path = "buffer_tests.rs"]
mod buffer_tests;
