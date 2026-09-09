//! Paced reveal of streamed text. Port of jcode `jcode-tui-core/src/stream_buffer.rs`
//! (see `jcode/crates/jcode-tui-core/src/stream_buffer.rs`): arrival and reveal are
//! decoupled; a proportional controller reveals `BASE_REVEAL_CPS + backlog *
//! REVEAL_BACKLOG_GAIN` chars/sec, capped at `MAX_REVEAL_CPS`, with the elapsed step
//! clamped to `MAX_REVEAL_STEP` so idle gaps cannot bank budget. Reasoning and answer
//! text are ordered segments of one stream.

use std::time::{Duration, Instant};

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

pub struct StreamBuffer {
    _private: (),
}

impl StreamBuffer {
    pub fn new(now: Instant) -> Self {
        let _ = now;
        todo!("StreamBuffer::new")
    }

    /// Queue arriving content.
    pub fn push(&mut self, kind: StreamKind, text: &str) {
        let _ = (kind, text);
        todo!("StreamBuffer::push")
    }

    /// Queue a zero-width "reasoning closed" marker.
    pub fn close_reasoning(&mut self) {
        todo!("StreamBuffer::close_reasoning")
    }

    /// Reveal what the pacing budget allows since the last call. Splits only on char
    /// boundaries (never inside a grapheme's base char sequence is not required).
    pub fn reveal(&mut self, now: Instant) -> Vec<StreamOp> {
        let _ = now;
        todo!("StreamBuffer::reveal")
    }

    /// Reveal everything immediately (message complete, interrupt).
    pub fn flush(&mut self) -> Vec<StreamOp> {
        todo!("StreamBuffer::flush")
    }

    /// Queued characters not yet revealed.
    pub fn backlog_chars(&self) -> usize {
        todo!("StreamBuffer::backlog_chars")
    }

    pub fn is_empty(&self) -> bool {
        self.backlog_chars() == 0
    }
}
