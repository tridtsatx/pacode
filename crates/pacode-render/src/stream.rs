//! Paced reveal of streamed text. Port of jcode `jcode-tui-core/src/stream_buffer.rs`
//! (see `jcode/crates/jcode-tui-core/src/stream_buffer.rs`): arrival and reveal are
//! decoupled; a proportional controller reveals `BASE_REVEAL_CPS + backlog *
//! REVEAL_BACKLOG_GAIN` chars/sec, capped at `MAX_REVEAL_CPS`, with the elapsed step
//! clamped to `MAX_REVEAL_STEP` so idle gaps cannot bank budget. Reasoning and answer
//! text are ordered segments of one stream.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;

pub const BASE_REVEAL_CPS: f32 = 180.0;
pub const REVEAL_BACKLOG_GAIN: f32 = 3.0;
pub const MAX_REVEAL_CPS: f32 = 960.0;
pub const MAX_REVEAL_STEP: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Text,
    Reasoning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamOp {
    Text(String),
    Reasoning(String),
    /// The live reasoning region ends here (exactly after its last revealed char).
    CloseReasoning,
}

#[derive(Debug)]
enum QueuedOp {
    Chunk { kind: StreamKind, text: String },
    CloseReasoning,
}

pub struct StreamBuffer {
    queue: VecDeque<QueuedOp>,
    backlog_chars: usize,
    last_reveal: Instant,
    carry: f32,
    ceiling_carry: f32,
    reasoning_open: bool,
}

impl StreamBuffer {
    pub fn new(now: Instant) -> Self {
        Self {
            queue: VecDeque::new(),
            backlog_chars: 0,
            last_reveal: now,
            carry: 0.0,
            ceiling_carry: 0.0,
            reasoning_open: false,
        }
    }

    /// Queue arriving content.
    pub fn push(&mut self, kind: StreamKind, text: &str) {
        if text.is_empty() {
            return;
        }
        match kind {
            StreamKind::Text => {
                if self.reasoning_open && !text.trim().is_empty() {
                    self.queue.push_back(QueuedOp::CloseReasoning);
                    self.reasoning_open = false;
                }
                self.push_chunk(StreamKind::Text, text);
            }
            StreamKind::Reasoning => {
                self.reasoning_open = true;
                self.push_chunk(StreamKind::Reasoning, text);
            }
        }
    }

    /// Queue a zero-width "reasoning closed" marker.
    pub fn close_reasoning(&mut self) {
        if self.reasoning_open {
            self.queue.push_back(QueuedOp::CloseReasoning);
            self.reasoning_open = false;
        }
    }

    /// Reveal what the pacing budget allows since the last call. Splits only on char
    /// boundaries (never inside a grapheme's base char sequence is not required).
    pub fn reveal(&mut self, now: Instant) -> Vec<StreamOp> {
        if self.backlog_chars == 0 {
            self.carry = 0.0;
            self.ceiling_carry = 0.0;
            self.last_reveal = now;
            return self.drain_ops(0, true);
        }

        let dt = now
            .saturating_duration_since(self.last_reveal)
            .min(MAX_REVEAL_STEP)
            .as_secs_f32();
        self.last_reveal = now;

        let cps = BASE_REVEAL_CPS + self.backlog_chars as f32 * REVEAL_BACKLOG_GAIN;
        self.carry += dt * cps;
        self.ceiling_carry += dt * MAX_REVEAL_CPS;

        let controller_budget = self.carry.floor() as usize;
        let ceiling_budget = self.ceiling_carry.floor() as usize;
        let mut reveal = controller_budget.min(ceiling_budget);
        if reveal == 0 {
            return self.drain_ops(0, false);
        }

        reveal = reveal.min(self.backlog_chars);
        self.carry -= reveal as f32;
        self.ceiling_carry -= reveal as f32;
        self.drain_ops(reveal, false)
    }

    /// Reveal everything immediately (message complete, interrupt).
    pub fn flush(&mut self) -> Vec<StreamOp> {
        self.carry = 0.0;
        self.ceiling_carry = 0.0;
        let ops = self.drain_ops(self.backlog_chars, true);
        self.queue.clear();
        self.backlog_chars = 0;
        self.reasoning_open = false;
        ops
    }

    /// Queued characters not yet revealed.
    pub fn backlog_chars(&self) -> usize {
        self.backlog_chars
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn push_chunk(&mut self, kind: StreamKind, text: &str) {
        self.backlog_chars += text.chars().count();
        if let Some(QueuedOp::Chunk {
            kind: last_kind,
            text: last_text,
        }) = self.queue.back_mut()
            && *last_kind == kind
        {
            last_text.push_str(text);
            return;
        }
        self.queue.push_back(QueuedOp::Chunk {
            kind,
            text: text.to_string(),
        });
    }

    fn drain_ops(&mut self, mut char_count: usize, drain_all_markers: bool) -> Vec<StreamOp> {
        let mut ops: Vec<StreamOp> = Vec::new();
        loop {
            match self.queue.front_mut() {
                None => break,
                Some(QueuedOp::CloseReasoning) => {
                    self.queue.pop_front();
                    ops.push(StreamOp::CloseReasoning);
                }
                Some(QueuedOp::Chunk { kind, text }) => {
                    if char_count == 0 {
                        let _ = drain_all_markers;
                        break;
                    }
                    let kind = *kind;
                    let available = text.chars().count();
                    let take = char_count.min(available);
                    let chunk = if take == available {
                        let op = self.queue.pop_front();
                        match op {
                            Some(QueuedOp::Chunk { text, .. }) => text,
                            _ => unreachable!(),
                        }
                    } else {
                        let end = text
                            .char_indices()
                            .nth(take)
                            .map(|(idx, _)| idx)
                            .unwrap_or(text.len());
                        let chunk = text[..end].to_string();
                        text.replace_range(..end, "");
                        chunk
                    };
                    char_count -= take;
                    self.backlog_chars = self.backlog_chars.saturating_sub(take);
                    match kind {
                        StreamKind::Text => ops.push(StreamOp::Text(chunk)),
                        StreamKind::Reasoning => ops.push(StreamOp::Reasoning(chunk)),
                    }
                }
            }
        }
        ops
    }
}
