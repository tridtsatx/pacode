//! Bounded head/tail output buffer. Port of codex `core/src/unified_exec/head_tail_buffer.rs`:
//! keeps the first `head_cap` bytes and the last `tail_cap` bytes, counts everything.

pub struct HeadTailBuffer {
    _private: (),
}

impl HeadTailBuffer {
    pub fn new(head_cap: usize, tail_cap: usize) -> Self {
        let _ = (head_cap, tail_cap);
        todo!("HeadTailBuffer::new")
    }

    pub fn push(&mut self, bytes: &[u8]) {
        let _ = bytes;
        todo!("HeadTailBuffer::push")
    }

    pub fn total_bytes(&self) -> u64 {
        todo!("HeadTailBuffer::total_bytes")
    }

    pub fn head(&self) -> &[u8] {
        todo!("HeadTailBuffer::head")
    }

    pub fn tail(&self) -> &[u8] {
        todo!("HeadTailBuffer::tail")
    }

    /// Lossy UTF-8 text: head, a `[... N bytes omitted ...]` marker when anything was
    /// dropped, tail. Then capped to `max_chars` (head+tail again).
    pub fn render(&self, max_chars: usize) -> String {
        let _ = max_chars;
        todo!("HeadTailBuffer::render")
    }

    /// Last `n` complete lines of the tail.
    pub fn tail_lines(&self, n: usize) -> Vec<String> {
        let _ = n;
        todo!("HeadTailBuffer::tail_lines")
    }
}
